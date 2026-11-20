//! Run Claude Code from Rust.
//!
//! Each [`Session`] keeps one `claude` process running (Claude Code CLI
//! 2.1.329 or newer). Send prompts with [`Agent`], approve or block tools with
//! hooks and [`CanUseTool`], give Claude tools written in Rust, run many
//! sessions with [`SessionPool`], and replay recorded sessions in tests.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use claude_code_agent::{Session, SessionOptions, Initialize, Agent, AgentEvent};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let session = Session::spawn(
//!         SessionOptions { cwd: Some("/my/project".into()), ..Default::default() },
//!         Initialize::default(),
//!     ).await?;
//!
//!     let agent = Agent::new(session);
//!     let mut stream = agent.prompt("hello").await?;
//!
//!     while let Some(event) = stream.next().await {
//!         match event {
//!             AgentEvent::Text(t) => print!("{}", t),
//!             AgentEvent::Done(r) => {
//!                 eprintln!("\ncost: ${:.4}", r.total_cost_usd.unwrap_or(0.0));
//!                 break;
//!             }
//!             AgentEvent::Error(e) => eprintln!("error: {}", e),
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # With hooks
//!
//! ```rust,no_run
//! use claude_code_agent::{Agent, AgentEvent, CanUseTool, HookRegistry, Initialize, Session, SessionOptions};
//! use claude_code_agent::protocol::{HookOutput, ToolDecision};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     // Hooks are declared before spawn: the CLI learns them from `initialize`.
//!     let mut hooks = HookRegistry::new();
//!     hooks.on_pre_tool_use(|payload| async move {
//!         match payload.tool_name.as_deref() {
//!             Some("Bash") => HookOutput::allow(),
//!             Some(name)   => HookOutput::block(format!("{name} not allowed")),
//!             None         => HookOutput::allow(),
//!         }
//!     });
//!
//!     let opts = SessionOptions {
//!         hooks,
//!         // Asked whenever the CLI's own permission rules would prompt.
//!         can_use_tool: Some(CanUseTool::new(|req| async move {
//!             if req.tool_name == "Bash" { ToolDecision::allow() } else { ToolDecision::deny("read-only") }
//!         })),
//!         ..Default::default()
//!     };
//!     let session = Session::spawn(opts, Initialize::default()).await?;
//!     let agent = Agent::new(session);
//!
//!     let mut stream = agent.prompt("list files in /tmp").await?;
//!     while let Some(ev) = stream.next().await {
//!         if let AgentEvent::Text(t) = &ev { print!("{}", t); }
//!         if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # With session pool (high throughput)
//!
//! ```rust,no_run
//! use claude_code_agent::{SessionPool, PoolConfig, Agent, AgentEvent};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let pool = SessionPool::new(PoolConfig { max_sessions: 4, ..Default::default() });
//!     pool.warm(2).await; // pre-warm 2 processes
//!
//!     // Parallel prompts — each reuses an idle session from the pool
//!     let (r1, r2) = tokio::join!(
//!         run_prompt(&pool, "task one"),
//!         run_prompt(&pool, "task two"),
//!     );
//!     Ok(())
//! }
//!
//! async fn run_prompt(pool: &SessionPool, text: &str) -> anyhow::Result<String> {
//!     let borrowed = pool.acquire().await?;
//!     let agent = Agent::new(borrowed.session.clone());
//!     let mut stream = agent.prompt(text).await?;
//!     let mut out = String::new();
//!     while let Some(ev) = stream.next().await {
//!         if let AgentEvent::Text(t) = &ev { out.push_str(t); }
//!         if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
//!     }
//!     Ok(out)
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod agent;
pub mod background;
pub mod code_mode;
pub mod compaction;
pub mod fs_events;
pub mod hooks;
pub mod mcp;
pub mod middleware;
pub mod pool;
pub mod protocol;
pub mod replay;
pub mod session;

// ── Top-level re-exports ───────────────────────────────────────────────────────

pub use agent::{Agent, AgentError, AgentEvent, AgentStream, TurnError, TurnResult};
pub use hooks::HookRegistry;
pub use mcp::{SdkMcpServer, SdkTool, ToolContent, ToolContext, ToolResult};
/// `#[claude_tool]` — enable with the `macros` feature.
#[cfg(feature = "macros")]
pub use claude_code_agent_macros::claude_tool;
pub use pool::{Checkout, PoolConfig, PooledSession, SessionPool};
pub use protocol::{
    // Core messages
    Initialize, UserMessage, ContentBlock, Priority,
    // Session settings
    Autocompact, Effort, PermissionMode, PermissionPrompts, Thinking, ThinkingDisplay,
    // Inbound
    InboundMessage, SystemMessage, AssistantMessage, ResultMessage, UsageSummary,
    BackgroundTask, TurnErrorKind,
    // Control
    ToolDecision, HookOutput, HookDecision, ControlRequest, ControlRequestBody,
    ControlResponse, CanUseToolRequest, ElicitationRequest, ElicitationResponse, UserDialogBehavior, UserDialogRequest, UserDialogResponse,
    // Hook types
    HookEventKind, HookEventPayload, HookCallbackRequest,
    // Tools/MCP
    ToolDef, ToolAnnotations, McpManifest,
    // Background
    BackgroundTasksRequest,
};
pub use session::{
    CanUseTool, McpCall, OnElicitation, OnStderr, OnUserDialog, ReloadPlugins, Session, SessionError,
    SessionOptions, WeakSession,
};

/// Re-exports used by code generated from `#[claude_tool]`. Not public API.
#[doc(hidden)]
pub mod __private {
    pub use serde_json;
}
