/// Agent layer — sends a turn and returns a streaming event iterator.
///
/// Control requests (hook callbacks, can_use_tool) are answered by the
/// `Session` itself; this layer only turns frames into `AgentEvent`s until
/// the turn's `result` arrives.
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::broadcast;

use crate::protocol::{AssistantBlock, BackgroundTask, InboundMessage, ResultMessage, UserMessage};
use crate::session::{Session, SessionError};

// ─── Event stream ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A text delta (requires --include-partial-messages OR full assistant message).
    Text(String),
    /// A complete thinking block.
    Thinking(String),
    /// A tool call the CLI is about to execute.
    ToolUse { id: String, name: String, input: serde_json::Value },
    /// Background task tree updated (replace semantics).
    BackgroundTasks(Vec<BackgroundTask>),
    /// Model was switched during the turn.
    ModelFallback { from: String, to: String, trigger: String },
    /// Context was compacted (`trigger`: "manual" for /compact, "auto").
    Compacted { trigger: String, pre_tokens: Option<u64>, post_tokens: Option<u64> },
    /// Turn completed successfully.
    Done(TurnResult),
    /// Turn failed.
    Error(String),
    /// Event broadcast lagged — some events were dropped.
    StreamLag { dropped: u64 },
}

#[derive(Debug, Clone)]
pub struct TurnResult {
    pub session_id: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub ttft_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl From<&ResultMessage> for TurnResult {
    fn from(r: &ResultMessage) -> Self {
        TurnResult {
            session_id: r.session_id.clone(),
            total_cost_usd: r.total_cost_usd,
            ttft_ms: r.ttft_ms,
            duration_ms: r.duration_ms,
            input_tokens: r.usage.as_ref().map(|u| u.input_tokens),
            output_tokens: r.usage.as_ref().map(|u| u.output_tokens),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("session error: {0}")]
    Session(#[from] SessionError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type AgentStream = Pin<Box<dyn tokio_stream::Stream<Item = AgentEvent> + Send + 'static>>;

// ─── Agent ────────────────────────────────────────────────────────────────────

/// Runs turns on a Session.
pub struct Agent {
    session: Session,
}

impl Agent {
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run a prompt and return a streaming iterator of AgentEvents.
    ///
    /// The stream ends after a `Done` or `Error` event.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<AgentStream, AgentError> {
        // Subscribe before sending so no frame of this turn is missed.
        let mut rx = self.session.subscribe();
        let message = UserMessage::text(text.into());
        let uuid = message.uuid.clone();
        self.session.send(&message).await?;
        // With partial messages on, text already streamed as deltas; the
        // complete assistant message would repeat it.
        let partial = self.session.include_partial();
        // Lives as long as the stream: its duration is the turn's duration.
        let span = tracing::info_span!(
            "claude.turn",
            session_id = self.session.session_id().unwrap_or(""),
            cost_usd = tracing::field::Empty,
            input_tokens = tracing::field::Empty,
            output_tokens = tracing::field::Empty,
            ttft_ms = tracing::field::Empty,
            error = tracing::field::Empty,
        );

        let stream = async_stream::stream! {
            // Turns run one after another and each ends with a `result`. Our
            // prompt's echo falls inside our turn: first for a normal prompt,
            // last for a slash command (after e.g. compact_boundary). So frames
            // since the previous `result` are held until the echo shows whose
            // turn they are; a `result` before our echo closes someone else's
            // turn (a queued prompt, or one the CLI started itself).
            let mut started = false;
            let mut held: Vec<Arc<InboundMessage>> = vec![];
            loop {
                let msg = match rx.recv().await {
                    Ok(m) => m,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        yield AgentEvent::StreamLag { dropped: n };
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        yield AgentEvent::Error("session closed before the turn finished".into());
                        break;
                    }
                };

                if !started {
                    match msg.as_ref() {
                        InboundMessage::User(u) if u.uuid.as_deref() == Some(uuid.as_str()) => {
                            started = true;
                            for m in held.drain(..) {
                                for ev in to_events(&m, partial) {
                                    yield ev;
                                }
                            }
                        }
                        InboundMessage::Result(_) => held.clear(),
                        _ => held.push(msg),
                    }
                    continue;
                }

                if let InboundMessage::Result(r) = msg.as_ref() {
                    let t = TurnResult::from(r);
                    if let Some(v) = t.total_cost_usd { span.record("cost_usd", v); }
                    if let Some(v) = t.input_tokens { span.record("input_tokens", v); }
                    if let Some(v) = t.output_tokens { span.record("output_tokens", v); }
                    if let Some(v) = t.ttft_ms { span.record("ttft_ms", v); }
                    if !r.is_success() { span.record("error", r.subtype.as_str()); }
                }
                let done = matches!(msg.as_ref(), InboundMessage::Result(_));
                for ev in to_events(&msg, partial) {
                    yield ev;
                }
                if done {
                    break;
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

/// The AgentEvents one frame of the prompt's own turn produces.
/// `partial`: text arrives as stream deltas, so complete assistant text is skipped.
fn to_events(msg: &InboundMessage, partial: bool) -> Vec<AgentEvent> {
    match msg {
        InboundMessage::Assistant(a) => a
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                AssistantBlock::Text { text } if !partial => Some(AgentEvent::Text(text.clone())),
                AssistantBlock::Thinking { thinking } if !partial => Some(AgentEvent::Thinking(thinking.clone())),
                AssistantBlock::ToolUse { id, name, input } => Some(AgentEvent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect(),

        InboundMessage::StreamEvent(se) if se.parent_tool_use_id.is_none() => {
            let delta = se.event.get("delta");
            let text = delta.and_then(|d| d.get("text")).and_then(|t| t.as_str());
            let thinking = delta.and_then(|d| d.get("thinking")).and_then(|t| t.as_str());
            text.map(|t| AgentEvent::Text(t.to_string()))
                .into_iter()
                .chain(thinking.map(|t| AgentEvent::Thinking(t.to_string())))
                .collect()
        }

        InboundMessage::System(s) => match s.subtype.as_str() {
            "background_tasks_changed" => {
                let tasks = s.extra.get("tasks").cloned().unwrap_or_default();
                match serde_json::from_value::<Vec<BackgroundTask>>(tasks) {
                    Ok(tasks) => vec![AgentEvent::BackgroundTasks(tasks)],
                    Err(e) => {
                        tracing::debug!(error = %e, "undecodable background_tasks_changed");
                        vec![]
                    }
                }
            }
            "model_fallback" => {
                let field = |k: &str| s.extra.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                vec![AgentEvent::ModelFallback {
                    from: field("original_model"),
                    to: field("fallback_model"),
                    trigger: field("trigger"),
                }]
            }
            "compact_boundary" => {
                let meta = s.extra.get("compact_metadata");
                let num = |k: &str| meta.and_then(|m| m.get(k)).and_then(|v| v.as_u64());
                vec![AgentEvent::Compacted {
                    trigger: meta.and_then(|m| m.get("trigger")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    pre_tokens: num("pre_tokens"),
                    post_tokens: num("post_tokens"),
                }]
            }
            _ => vec![],
        },

        InboundMessage::Result(r) if r.is_success() => vec![AgentEvent::Done(TurnResult::from(r))],
        InboundMessage::Result(r) => {
            vec![AgentEvent::Error(r.error.clone().unwrap_or_else(|| r.subtype.clone()))]
        }

        _ => vec![],
    }
}

// Re-export TurnStream as a type alias for external use.
pub type TurnStream = AgentStream;
