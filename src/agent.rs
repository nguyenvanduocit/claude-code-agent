//! Agent layer — sends a turn and returns a streaming event iterator.
//!
//! Control requests (hook callbacks, `can_use_tool`) are answered by the
//! `Session` itself; this layer only turns frames into `AgentEvent`s.
//! `prompt` yields the events of one turn; `watch` yields every turn's,
//! including the turns the CLI starts on its own.
use std::pin::Pin;
use std::sync::Arc;

use crate::protocol::{
    AssistantBlock, BackgroundTask, InboundMessage, InboundUserMessage, ResultMessage, TurnErrorKind,
    UserMessage,
};
use crate::session::{Session, SessionError};

// ─── Event stream ─────────────────────────────────────────────────────────────

/// One event of a turn, translated from the CLI's stdout frames.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// A text delta (requires --include-partial-messages OR full assistant message).
    Text(String),
    /// A complete thinking block.
    Thinking(String),
    /// Encrypted thinking, opaque to the host.
    RedactedThinking(String),
    /// A tool call: one the CLI runs, or one the API runs itself
    /// (`server: true`, e.g. web search).
    ToolUse {
        /// Tool call id; matches `ToolResult::tool_use_id`.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input, as the model wrote it.
        input: serde_json::Value,
        /// `true` for a server tool call (`server_tool_use` block).
        server: bool,
    },
    /// The result of a tool call. `content` is a string or content blocks,
    /// as sent to the model; `structured` is the tool's full output when
    /// the CLI reports one (e.g. Bash `{stdout, stderr, interrupted, …}`).
    ToolResult {
        /// Id of the `ToolUse` this result answers.
        tool_use_id: String,
        /// Result sent to the model: a string or content blocks.
        content: serde_json::Value,
        /// The tool call failed.
        is_error: bool,
        /// The tool's full output (`tool_use_result`), when reported.
        structured: Option<serde_json::Value>,
    },
    /// An event of a subagent, started by the `Agent`/`Task` tool call
    /// `parent_tool_use_id`.
    Subagent {
        /// Id of the `Agent`/`Task` tool call that started the subagent.
        parent_tool_use_id: String,
        /// The subagent's event.
        event: Box<AgentEvent>,
    },
    /// Background task tree updated (replace semantics).
    BackgroundTasks(Vec<BackgroundTask>),
    /// Model was switched during the turn.
    ModelFallback {
        /// Model before the switch (`original_model`).
        from: String,
        /// Model after the switch (`fallback_model`).
        to: String,
        /// The frame's `trigger` field.
        trigger: String,
    },
    /// Context was compacted (`trigger`: "manual" for /compact, "auto").
    Compacted {
        /// "manual" or "auto".
        trigger: String,
        /// Context tokens before compaction.
        pre_tokens: Option<u64>,
        /// Context tokens after compaction.
        post_tokens: Option<u64>,
    },
    /// Any other `system` frame (`init`, `api_retry`, `task_started`,
    /// `hook_response`, `permission_denied`, `status`, …), verbatim.
    System {
        /// The frame's `subtype`.
        subtype: String,
        /// The whole frame.
        data: serde_json::Value,
    },
    /// Any other frame (`rate_limit_event`, `tool_progress`,
    /// `prompt_suggestion`, `command_lifecycle`, …), verbatim.
    Other {
        /// The frame's `type`.
        kind: String,
        /// The whole frame.
        data: serde_json::Value,
    },
    /// Turn completed successfully.
    Done(TurnResult),
    /// Turn failed.
    Error(TurnError),
}

/// Outcome of a finished turn, from its `result` frame.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct TurnResult {
    /// Session id the CLI reported.
    pub session_id: Option<String>,
    /// Final text of the turn.
    pub result: Option<String>,
    /// Output matching `Initialize::json_schema`.
    pub structured_output: Option<serde_json::Value>,
    /// Cost in USD (`total_cost_usd`).
    pub total_cost_usd: Option<f64>,
    /// Time to first token, in milliseconds (`ttft_ms`).
    pub ttft_ms: Option<u64>,
    /// Wall-clock duration of the turn, in milliseconds (`duration_ms`).
    pub duration_ms: Option<u64>,
    /// Time spent in API calls, in milliseconds (`duration_api_ms`).
    pub duration_api_ms: Option<u64>,
    /// Agentic turns (model calls) the turn took.
    pub num_turns: Option<u64>,
    /// `usage.input_tokens`.
    pub input_tokens: Option<u64>,
    /// `usage.output_tokens`.
    pub output_tokens: Option<u64>,
    /// `usage.cache_read_input_tokens`.
    pub cache_read_input_tokens: Option<u64>,
    /// `usage.cache_creation_input_tokens`.
    pub cache_creation_input_tokens: Option<u64>,
    /// Per-model token and cost breakdown.
    pub model_usage: Option<serde_json::Value>,
    /// `stop_reason` of the `result` frame.
    pub stop_reason: Option<String>,
    /// Why the agent loop ended (`terminal_reason`, e.g. "completed").
    pub terminal_reason: Option<String>,
    /// Tool calls the permission system denied.
    pub permission_denials: Vec<serde_json::Value>,
    /// Set when the CLI started the turn itself (e.g. after a background
    /// task finished); `None` for prompted turns.
    pub origin: Option<serde_json::Value>,
}

impl From<&ResultMessage> for TurnResult {
    fn from(r: &ResultMessage) -> Self {
        let usage = r.usage.as_ref();
        TurnResult {
            session_id: r.session_id.clone(),
            result: r.result.clone(),
            structured_output: r.structured_output.clone(),
            total_cost_usd: r.total_cost_usd,
            ttft_ms: r.ttft_ms,
            duration_ms: r.duration_ms,
            duration_api_ms: r.duration_api_ms,
            num_turns: r.num_turns,
            input_tokens: usage.map(|u| u.input_tokens),
            output_tokens: usage.map(|u| u.output_tokens),
            cache_read_input_tokens: usage.and_then(|u| u.cache_read_input_tokens),
            cache_creation_input_tokens: usage.and_then(|u| u.cache_creation_input_tokens),
            model_usage: r.model_usage.clone(),
            stop_reason: r.stop_reason.clone(),
            terminal_reason: r.terminal_reason.clone(),
            permission_denials: r.permission_denials.clone(),
            origin: r.origin.clone(),
        }
    }
}

/// Why a turn failed. `result` carries cost and usage up to the failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct TurnError {
    /// Failure category.
    pub kind: TurnErrorKind,
    /// Human-readable error message.
    pub message: String,
    /// Usage up to the failure; `None` when the turn never ran or the
    /// session closed.
    pub result: Option<TurnResult>,
}

impl From<&ResultMessage> for TurnError {
    fn from(r: &ResultMessage) -> Self {
        TurnError { kind: r.error_kind(), message: r.error_message(), result: Some(TurnResult::from(r)) }
    }
}

impl TurnError {
    fn cancelled() -> Self {
        TurnError { kind: TurnErrorKind::Cancelled, message: "prompt cancelled before it ran".into(), result: None }
    }

    fn session_closed() -> Self {
        TurnError {
            kind: TurnErrorKind::SessionClosed,
            message: "session closed before the turn finished".into(),
            result: None,
        }
    }
}

/// Error starting a turn.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The session failed to send the prompt.
    #[error("session error: {0}")]
    Session(#[from] SessionError),
}

/// Events of one turn; ends after `Done` or `Error`. Dropping it before
/// then cancels the turn (see `Agent::prompt_message`).
pub type AgentStream = Pin<Box<dyn tokio_stream::Stream<Item = AgentEvent> + Send + 'static>>;

// ─── Agent ────────────────────────────────────────────────────────────────────

/// Runs turns on a Session.
pub struct Agent {
    session: Session,
}

impl Agent {
    /// Wrap `session`.
    #[must_use]
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    /// The underlying session.
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run a text prompt and return a streaming iterator of `AgentEvents`.
    ///
    /// The stream ends after a `Done` or `Error` event.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<AgentStream, AgentError> {
        self.prompt_message(UserMessage::text(text.into())).await
    }

    /// Run a prompt built from content blocks (text, images, …).
    ///
    /// Dropping the stream before its `Done`/`Error` cancels the prompt:
    /// still queued, it is withdrawn; running, its turn is interrupted.
    /// Drain the stream to let the turn finish.
    pub async fn prompt_message(&self, message: UserMessage) -> Result<AgentStream, AgentError> {
        // Subscribe before sending so no frame of this turn is missed.
        let mut rx = self.session.subscribe();
        let uuid = message.uuid.clone();
        self.session.send_prompt(&message).await?;
        // With partial messages on, text already streamed as deltas; the
        // complete assistant message would repeat it.
        let partial = self.session.include_partial();
        // Lives as long as the stream: its duration is the turn's duration.
        let span = tracing::info_span!(
            "claude.turn",
            session_id = self.session.session_id().unwrap_or_default(),
            cost_usd = tracing::field::Empty,
            input_tokens = tracing::field::Empty,
            output_tokens = tracing::field::Empty,
            ttft_ms = tracing::field::Empty,
            error = tracing::field::Empty,
        );

        // Created outside the generator: a stream dropped before its first
        // poll still cancels. It also keeps the session alive until the turn
        // ends, even when the Agent and every other handle are dropped first.
        let mut guard = CancelOnDrop { session: self.session.clone(), uuid: uuid.clone(), armed: true };
        let stream = async_stream::stream! {
            // Turns run one after another and each ends with a `result`. Our
            // prompt's echo falls inside our turn: first for a normal prompt,
            // last for a slash command (after e.g. compact_boundary). So frames
            // since the previous `result` are held until the echo shows whose
            // turn they are. A `result` before our echo closes someone else's
            // turn (a queued prompt, or one the CLI started itself) unless it
            // names our prompt (a prompt refused up front is never echoed).
            let mut started = false;
            let mut held: Vec<Arc<InboundMessage>> = vec![];
            loop {
                let Some(msg) = rx.recv().await else {
                    guard.disarm();
                    yield AgentEvent::Error(TurnError::session_closed());
                    break;
                };
                // Withdrawn before it ran (`cancel_prompt`, `interrupt_all`):
                // no `result` will come.
                if let InboundMessage::Other(frame) = msg.as_ref()
                    && is_cancelled(frame, &uuid)
                {
                    guard.disarm();
                    yield AgentEvent::Error(TurnError::cancelled());
                    break;
                }

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
                        // Refused before it started: no echo, but the
                        // result names our prompt. The held frames are ours.
                        InboundMessage::Result(r) if r.answers(&uuid) => {
                            started = true;
                            for m in held.drain(..) {
                                for ev in to_events(&m, partial) {
                                    yield ev;
                                }
                            }
                        }
                        InboundMessage::Result(_) => {
                            held.clear();
                            continue;
                        }
                        _ => {
                            held.push(msg);
                            continue;
                        }
                    }
                    if !matches!(msg.as_ref(), InboundMessage::Result(_)) {
                        continue;
                    }
                }

                if let InboundMessage::Result(r) = msg.as_ref() {
                    guard.disarm();
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

    /// Every event of every turn from now on — prompted turns and the turns
    /// the CLI starts itself (`TurnResult::origin` set), each ending with
    /// `Done` or `Error`. Ends when the session closes. Dropping it cancels
    /// nothing. Lossless like `Session::subscribe`: events buffer until
    /// polled, so poll it or drop it.
    pub fn watch(&self) -> AgentStream {
        let mut rx = self.session.subscribe();
        let partial = self.session.include_partial();
        Box::pin(async_stream::stream! {
            while let Some(msg) = rx.recv().await {
                for ev in to_events(&msg, partial) {
                    yield ev;
                }
            }
        })
    }
}

/// Cancels the prompt `uuid` unless disarmed (its turn ended).
struct CancelOnDrop {
    session: Session,
    uuid: String,
    armed: bool,
}

impl CancelOnDrop {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed || !self.session.is_alive() {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("turn stream dropped outside a Tokio runtime; the turn keeps running");
            return;
        };
        let session = self.session.clone();
        let uuid = std::mem::take(&mut self.uuid);
        rt.spawn(async move {
            if let Err(e) = session.cancel_prompt(&uuid).await {
                tracing::debug!(error = %e, "cancelling a dropped turn failed");
            }
        });
    }
}

/// The `AgentEvents` one frame produces. `partial`: text arrives as stream
/// deltas, so complete main-thread text is skipped. Frames of subagents
/// (`parent_tool_use_id` set) are wrapped in `AgentEvent::Subagent`; their
/// text comes from complete messages only.
fn to_events(msg: &InboundMessage, partial: bool) -> Vec<AgentEvent> {
    match msg {
        InboundMessage::Assistant(a) => {
            let sub = a.parent_tool_use_id.is_some();
            let events = a.message.content.iter().filter_map(|block| match block {
                AssistantBlock::Text { text } if sub || !partial => Some(AgentEvent::Text(text.clone())),
                AssistantBlock::Thinking { thinking } if sub || !partial => {
                    Some(AgentEvent::Thinking(thinking.clone()))
                }
                AssistantBlock::RedactedThinking { data } => Some(AgentEvent::RedactedThinking(data.clone())),
                AssistantBlock::ToolUse { id, name, input } => Some(AgentEvent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    server: false,
                }),
                AssistantBlock::ServerToolUse { id, name, input } => Some(AgentEvent::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                    server: true,
                }),
                AssistantBlock::WebSearchToolResult { tool_use_id, content } => Some(AgentEvent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: content.clone(),
                    is_error: false,
                    structured: None,
                }),
                _ => None,
            });
            in_thread(a.parent_tool_use_id.as_deref(), events.collect())
        }

        InboundMessage::User(u) => in_thread(u.parent_tool_use_id.as_deref(), tool_results(u)),

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
                let num = |k: &str| meta.and_then(|m| m.get(k)).and_then(serde_json::Value::as_u64);
                vec![AgentEvent::Compacted {
                    trigger: meta.and_then(|m| m.get("trigger")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                    pre_tokens: num("pre_tokens"),
                    post_tokens: num("post_tokens"),
                }]
            }
            other => vec![AgentEvent::System {
                subtype: other.to_string(),
                data: serde_json::to_value(s).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "could not serialize a system frame");
                    serde_json::Value::Null
                }),
            }],
        },

        InboundMessage::Other(data) => vec![AgentEvent::Other {
            kind: data.get("type").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            data: data.clone(),
        }],

        InboundMessage::Result(r) if r.is_success() => vec![AgentEvent::Done(TurnResult::from(r))],
        InboundMessage::Result(r) => vec![AgentEvent::Error(TurnError::from(r))],

        _ => vec![],
    }
}

/// The `ToolResult` events of a `user` frame. The frame's
/// `tool_use_result` is the structured output of its (single) tool result.
fn tool_results(u: &InboundUserMessage) -> Vec<AgentEvent> {
    let results = u.tool_results();
    let structured = if results.len() == 1 { u.tool_use_result.clone() } else { None };
    results
        .into_iter()
        .map(|r| AgentEvent::ToolResult {
            tool_use_id: r.tool_use_id,
            content: r.content,
            is_error: r.is_error,
            structured: structured.clone(),
        })
        .collect()
}

/// Whether `frame` is the CLI's `command_lifecycle` "cancelled" for prompt `uuid`.
fn is_cancelled(frame: &serde_json::Value, uuid: &str) -> bool {
    let field = |k: &str| frame.get(k).and_then(|v| v.as_str());
    field("type") == Some("command_lifecycle")
        && field("state") == Some("cancelled")
        && field("command_uuid").is_some_and(|u| u.eq_ignore_ascii_case(uuid))
}

/// Wrap `events` as subagent events when `parent` is set.
fn in_thread(parent: Option<&str>, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    match parent {
        None => events,
        Some(p) => events
            .into_iter()
            .map(|event| AgentEvent::Subagent { parent_tool_use_id: p.to_string(), event: Box::new(event) })
            .collect(),
    }
}
