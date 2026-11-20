# claude-sdk

Full-protocol Rust SDK for [Claude Code](https://claude.ai/claude-code) — persistent subprocess, all 33 hook types, streaming, Code Mode, and session pooling.

## Why

Community Rust SDKs spawn a new `claude` process per request and expose 4–6 message types. This SDK uses the full internal protocol from Claude Code 2.1.328:

| Feature | Other SDKs | claude-sdk |
|---|---|---|
| Process lifecycle | spawn → kill per request | **Persistent subprocess** |
| Hook coverage | 4–6 types | **All 33 types** |
| Hook implementation | Shell commands | **Async Rust closures** |
| Streaming | None | **Real `stream_event` chunks** |
| Tool permission | None | **`on_can_use_tool` handler** |
| Session pool | None | **N pre-warmed processes** |
| Cost tracking | None | **`total_cost_usd`, `ttft_ms`** |
| Code Mode | None | **`code_mode: true`** |
| Reconnect | None | **`Session::reconnect(id)`** |

## Quick start

```rust
use claude_sdk::{Session, SessionOptions, Initialize, Agent, AgentEvent};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session = Session::spawn(
        SessionOptions::default(),
        Initialize::new("."),
    ).await?;

    let agent = Agent::new(session);
    let mut stream = agent.prompt("hello").await?;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text(t) => print!("{}", t),
            AgentEvent::Done(r) => {
                eprintln!("\ncost: ${:.4}", r.total_cost_usd.unwrap_or(0.0));
                break;
            }
            _ => {}
        }
    }
    Ok(())
}
```

## Hooks — async Rust closures on any of the 33 events

```rust
use claude_sdk::protocol::{HookEventKind, HookOutput};
use claude_sdk::hooks::HookRegistryExt;

// Block specific tools
session.hooks().on_pre_tool_use(|payload| async move {
    match payload.tool_name.as_deref() {
        Some("Bash") => HookOutput::allow(),
        Some(name)   => HookOutput::block(format!("{name} not allowed")),
        None         => HookOutput::allow(),
    }
}).await;

// Inject context into every subagent
session.hooks().on(HookEventKind::SubagentStart, None, |payload| async move {
    HookOutput::with_context("Budget: 5 tool calls max.")
}).await;

// Custom compact summary
session.hooks().on(HookEventKind::PreCompact, None, |payload| async move {
    HookOutput::compact_instructions("Preserve: current task, file paths, error messages.")
}).await;
```

## Session pool — high throughput

```rust
use claude_sdk::{SessionPool, PoolConfig, Agent, AgentEvent};

let pool = SessionPool::new(PoolConfig { max_sessions: 4, ..Default::default() });
pool.warm(2, ".").await; // pre-warm 2 processes

// N parallel prompts — no spawn overhead on reuse
let (r1, r2) = tokio::join!(
    run(&pool, "task one"),
    run(&pool, "task two"),
);
```

## `#[claude_tool]` proc macro

```rust
use claude_sdk_macros::claude_tool;

/// Search files matching a glob pattern
#[claude_tool]
async fn search_files(pattern: String, max_results: u64) -> anyhow::Result<Vec<String>> {
    // implementation
}

// Generates: search_files_tool_def() -> ToolDef
let mut init = Initialize::new(".");
init.tools.push(search_files_tool_def());
```

Enable with `claude-sdk = { features = ["macros"] }`.

## 33 hook types

`PreToolUse` · `PostToolUse` · `PostToolUseFailure` · `PostToolBatch` · `UserPromptSubmit` · `UserPromptExpansion` · `SessionStart` · `SessionEnd` · `Stop` · `StopFailure` · `SubagentStart` · `SubagentStop` · `PreCompact` · `PostCompact` · `PreModelSwitch` · `PostModelSwitch` · `PermissionRequest` · `PermissionDenied` · `Setup` · `TeammateIdle` · `TaskCreated` · `TaskCompleted` · `Elicitation` · `ElicitationResult` · `ConfigChange` · `WorktreeCreate` · `WorktreeRemove` · `InstructionsLoaded` · `CwdChanged` · `FileChanged` · `DirectoryAdded` · `MessageDisplay` · `Notification`

## License

MIT
