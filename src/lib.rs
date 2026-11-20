//! **claude-sdk** — Full-protocol Rust SDK for Claude Code.
//!
//! # What makes this different
//!
//! | Feature | Community SDKs | claude-sdk |
//! |---|---|---|
//! | Process lifecycle | spawn → result → kill | Persistent subprocess, one process per session |
//! | Hook coverage | 4–6 events | All 33 hook types with deadline enforcement |
//! | Streaming | None / fake | Real `stream_event` chunks via `--include-partial-messages` |
//! | Hook type | Shell commands only | Async Rust closures via `hook_callback` IPC |
//! | Tool permission | None | `on_can_use_tool` async handler |
//! | Session pool | None | N pre-warmed processes, auto-reuse, auto-respawn |
//! | Code Mode | None | `SessionOptions::code_mode = true` |
//! | Reconnect | None | `Session::reconnect(previous_id, ...)` |
//! | Cost tracking | None | `TurnResult::total_cost_usd`, `ttft_ms`, `duration_ms` |
//!
//! # Quick start
//!
//! ```rust,no_run
//! use claude_sdk::{Session, SessionOptions, Initialize, Agent, AgentEvent};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let session = Session::spawn(
//!         SessionOptions::default(),
//!         Initialize::new("/my/project"),
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
//! use claude_sdk::{Session, SessionOptions, Initialize, Agent, AgentEvent};
//! use claude_sdk::protocol::{HookEventKind, HookOutput, ToolDecision};
//! use claude_sdk::hooks::HookRegistryExt;
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let session = Session::spawn(SessionOptions::default(), Initialize::new(".")).await?;
//!
//!     // Register hook — allow only Bash
//!     session.hooks().on_pre_tool_use(|payload| async move {
//!         match payload.tool_name.as_deref() {
//!             Some("Bash") => HookOutput::allow(),
//!             Some(name)   => HookOutput::block(format!("{name} not allowed")),
//!             None         => HookOutput::allow(),
//!         }
//!     }).await;
//!
//!     let mut agent = Agent::new(session);
//!
//!     // Custom tool permission handler
//!     agent.on_can_use_tool(|name, input| async move {
//!         ToolDecision::Allow { updated_input: None, tool_use_id: None }
//!     });
//!
//!     let mut stream = agent.prompt("list files in /tmp").await?;
//!     while let Some(ev) = stream.next().await {
//!         if let AgentEvent::Text(t) = ev { print!("{}", t); }
//!         if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # With session pool (high throughput)
//!
//! ```rust,no_run
//! use claude_sdk::{SessionPool, PoolConfig, Agent, AgentEvent};
//! use tokio_stream::StreamExt;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let pool = SessionPool::new(PoolConfig { max_sessions: 4, ..Default::default() });
//!     pool.warm(2, ".").await; // pre-warm 2 processes
//!
//!     // Parallel prompts — each reuses an idle session from the pool
//!     let (r1, r2) = tokio::join!(
//!         run_prompt(&pool, "task one"),
//!         run_prompt(&pool, "task two"),
//!     );
//!     Ok(())
//! }
//!
//! async fn run_prompt(pool: &SessionPool, text: &str) -> String {
//!     let borrowed = pool.acquire(".").await;
//!     let agent = Agent::new(borrowed.session.clone());
//!     let mut stream = agent.prompt(text).await.unwrap();
//!     let mut out = String::new();
//!     while let Some(ev) = stream.next().await {
//!         if let AgentEvent::Text(t) = ev { out.push_str(&t); }
//!         if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
//!     }
//!     out
//! }
//! ```

pub mod agent;
pub mod background;
pub mod compaction;
pub mod fs_events;
pub mod hooks;
pub mod middleware;
pub mod otel;
pub mod pool;
pub mod protocol;
pub mod replay;
pub mod session;

// ── Top-level re-exports ───────────────────────────────────────────────────────

pub use agent::{Agent, AgentError, AgentEvent, TurnResult, TurnStream};
pub use hooks::{HookRegistry, HookRegistryExt, AllowAll, HookDispatcher};
pub use pool::{PoolConfig, PooledSession, SessionPool};
pub use protocol::{
    // Core messages
    Initialize, UserMessage, ContentBlock,
    // Inbound
    InboundMessage, SystemMessage, AssistantMessage, ResultMessage, UsageSummary,
    BackgroundTask, CompactBoundaryMessage,
    // Control
    ToolDecision, HookOutput, ControlRequest, ControlResponse,
    // Hook types
    HookEventKind, HookEventPayload, HookCallbackRequest,
    // Tools/MCP
    ToolDef, McpManifest,
    // Background
    BackgroundTasksRequest,
};
pub use session::{Session, SessionError, SessionOptions, StartupMark};
