/// Agent layer — sends a turn and returns a streaming event iterator.
///
/// Responsibilities:
///   - send UserMessage
///   - forward hook_callback requests to the session's HookRegistry
///   - forward can_use_tool requests to the ToolPermission handler
///   - yield AgentEvents until Result::success/error arrives
///
/// Source: chunk-f89nsf5q.query.js (overall turn loop)
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;

use crate::protocol::{
    AssistantBlock, BackgroundTask, ControlRequestPayload, ControlResponse,
    ControlResponsePayload, HookCallbackReply, HookOutput, InboundMessage, ResultMessage,
    ToolDecision, UserMessage,
};
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
    ModelFallback { from: Option<String>, to: String },
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

// ─── Tool permission handler ──────────────────────────────────────────────────

type PermFuture = Pin<Box<dyn Future<Output = ToolDecision> + Send + 'static>>;
type PermFn = Arc<dyn Fn(String, serde_json::Value) -> PermFuture + Send + Sync>;

/// Wraps a Session and adds tool-permission and hook handling for a single turn.
pub struct Agent {
    session: Session,
    perm_handler: Option<PermFn>,
}

impl Agent {
    pub fn new(session: Session) -> Self {
        Self { session, perm_handler: None }
    }

    /// Register a custom tool-permission handler (replaces the default allow-all).
    ///
    /// ```rust
    /// agent.on_can_use_tool(|name, input| async move {
    ///     if name == "Bash" && input["command"].as_str().unwrap_or("").contains("rm") {
    ///         ToolDecision::Deny { message: "rm not allowed".into(), interrupt: None, tool_use_id: None }
    ///     } else {
    ///         ToolDecision::Allow { updated_input: None, tool_use_id: None }
    ///     }
    /// });
    /// ```
    pub fn on_can_use_tool<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: Fn(String, serde_json::Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolDecision> + Send + 'static,
    {
        self.perm_handler = Some(Arc::new(move |name, input| Box::pin(f(name, input))));
        self
    }

    /// Run a prompt and return a streaming iterator of AgentEvents.
    ///
    /// The stream ends when a `Done` or `Error` event is emitted.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<AgentStream, AgentError> {
        let msg = UserMessage::text(text.into());
        self.session.send(&msg).await?;

        let mut rx = self.session.subscribe();
        let session = self.session.clone();
        let perm_handler = self.perm_handler.clone();
        let hook_registry = session.hooks();

        let stream = async_stream::stream! {
            loop {
                let msg = match rx.recv().await {
                    Ok(m) => m,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        yield AgentEvent::StreamLag { dropped: n };
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };

                match msg.as_ref() {
                    // ── text from complete assistant message ──────────────────
                    InboundMessage::Assistant(a) => {
                        for block in &a.message.content {
                            match block {
                                AssistantBlock::Text { text } => {
                                    yield AgentEvent::Text(text.clone());
                                }
                                AssistantBlock::Thinking { thinking } => {
                                    yield AgentEvent::Thinking(thinking.clone());
                                }
                                AssistantBlock::ToolUse { id, name, input } => {
                                    yield AgentEvent::ToolUse {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: input.clone(),
                                    };
                                }
                                AssistantBlock::Unknown => {}
                            }
                        }
                    }

                    // ── partial streaming chunk ───────────────────────────────
                    InboundMessage::StreamEvent(se) => {
                        if let Some(delta) = se.event.get("delta") {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                yield AgentEvent::Text(text.to_string());
                            }
                            if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                                yield AgentEvent::Thinking(thinking.to_string());
                            }
                        }
                    }

                    // ── tool permission ───────────────────────────────────────
                    InboundMessage::ControlRequest(cr) if cr.subtype == "can_use_tool" => {
                        let decision = if let ControlRequestPayload::CanUseTool(ref req) = cr.payload {
                            if let Some(ref handler) = perm_handler {
                                handler(req.tool_name.clone(), req.tool_input.clone()).await
                            } else {
                                ToolDecision::Allow { updated_input: None, tool_use_id: req.tool_use_id.clone() }
                            }
                        } else {
                            ToolDecision::Allow { updated_input: None, tool_use_id: None }
                        };

                        let response = ControlResponse {
                            kind: "control_response".into(),
                            request_id: cr.request_id.clone(),
                            subtype: "can_use_tool".into(),
                            payload: ControlResponsePayload::CanUseTool(decision),
                        };
                        let _ = session.send(&response).await;
                    }

                    // ── hook callback ─────────────────────────────────────────
                    InboundMessage::ControlRequest(cr) if cr.subtype == "hook_callback" => {
                        if let ControlRequestPayload::HookCallback(ref hcb) = cr.payload {
                            let deadline_duration = Duration::from_millis(hcb.deadline_ms);
                            let issued = hcb.issued_at;
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            let elapsed = now_ms.saturating_sub(issued);
                            let remaining = deadline_duration
                                .saturating_sub(Duration::from_millis(elapsed));

                            let output = tokio::time::timeout(
                                remaining,
                                hook_registry.dispatch(&hcb.callback_id, &hcb.input),
                            ).await.unwrap_or_else(|_| HookOutput::allow());

                            let response = ControlResponse {
                                kind: "control_response".into(),
                                request_id: cr.request_id.clone(),
                                subtype: "hook_callback".into(),
                                payload: ControlResponsePayload::HookCallback(HookCallbackReply {
                                    callback_id: hcb.callback_id.clone(),
                                    output,
                                }),
                            };
                            let _ = session.send(&response).await;
                        }
                    }

                    // ── background task update ────────────────────────────────
                    InboundMessage::BackgroundTasksChanged(bt) => {
                        yield AgentEvent::BackgroundTasks(bt.tasks.clone());
                    }

                    // ── model fallback ────────────────────────────────────────
                    InboundMessage::ModelFallback { from_model, to_model } => {
                        yield AgentEvent::ModelFallback {
                            from: from_model.clone(),
                            to: to_model.clone(),
                        };
                    }

                    // ── turn complete ─────────────────────────────────────────
                    InboundMessage::Result(r) => {
                        if r.is_success() {
                            yield AgentEvent::Done(TurnResult::from(r));
                        } else {
                            let err = r.error.clone().unwrap_or_else(|| r.subtype.clone());
                            yield AgentEvent::Error(err);
                        }
                        break;
                    }

                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

// Re-export TurnStream as a type alias for external use.
pub type TurnStream = AgentStream;
