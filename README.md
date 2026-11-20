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
| Tool permission | None | **`SessionOptions::can_use_tool` handler** |
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
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await?;

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

Hooks are declared before spawn — the CLI learns them from the `initialize` request.

```rust
use claude_sdk::{CanUseTool, HookRegistry, Initialize, Session, SessionOptions};
use claude_sdk::protocol::{HookEventKind, HookOutput, ToolDecision};

let mut hooks = HookRegistry::new();

// Block a tool (matcher = tool-name regex)
hooks.on(HookEventKind::PreToolUse, Some("Bash"), |payload| async move {
    HookOutput::block("Bash is disabled")
});

// Inject context into every subagent
hooks.on(HookEventKind::SubagentStart, None, |payload| async move {
    HookOutput::with_context(HookEventKind::SubagentStart, "Budget: 5 tool calls max.")
});

let opts = SessionOptions {
    hooks,
    // Asked whenever the CLI's permission rules would prompt (--permission-prompt-tool stdio)
    can_use_tool: Some(CanUseTool::new(|req| async move {
        ToolDecision::deny(format!("{} denied", req.tool_name))
    })),
    ..Default::default()
};
let session = Session::spawn(opts, Initialize::default()).await?;
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

## Rust functions as tools — `#[claude_tool]` + SDK MCP server

```rust
use claude_sdk::{claude_tool, SdkMcpServer, Session, SessionOptions, Initialize};

/// Search files matching a glob pattern
#[claude_tool]
async fn search_files(pattern: String, max_results: u64) -> anyhow::Result<Vec<String>> {
    // implementation
}

let opts = SessionOptions {
    mcp_servers: vec![SdkMcpServer::new("files").tool(search_files_tool())],
    ..Default::default()
};
let session = Session::spawn(opts, Initialize::default()).await?;
// Claude now sees `mcp__files__search_files`; calls run in-process.
```

The macro generates `search_files_tool_def() -> ToolDef` and `search_files_tool() -> SdkTool`.
Arguments are decoded with serde; `Result<T, E>` maps to a result or an error result.
SDK tools are added to `--allowedTools` automatically. Enable with `claude-sdk = { features = ["macros"] }`.

## Record / replay — tests without a process or API cost

```rust
let opts = SessionOptions { record: Some("hi.jsonl".into()), ..Default::default() };
// … run a live session once …
let session = claude_sdk::replay::replay("hi.jsonl", SessionOptions::default()).await?;
```

## Compaction

```rust
let mut stream = claude_sdk::compaction::compact(&agent, Some("keep file paths and errors")).await?;
// yields AgentEvent::Compacted { trigger: "manual", pre_tokens, post_tokens }, then Done
```

## Observability

`tracing` spans `claude.turn`, `claude.hook`, `claude.tool` — export with any subscriber (e.g. `tracing-opentelemetry`).

## 33 hook types

`PreToolUse` · `PostToolUse` · `PostToolUseFailure` · `PostToolBatch` · `UserPromptSubmit` · `UserPromptExpansion` · `SessionStart` · `SessionEnd` · `Stop` · `StopFailure` · `SubagentStart` · `SubagentStop` · `PreCompact` · `PostCompact` · `PreModelSwitch` · `PostModelSwitch` · `PermissionRequest` · `PermissionDenied` · `Setup` · `TeammateIdle` · `TaskCreated` · `TaskCompleted` · `Elicitation` · `ElicitationResult` · `ConfigChange` · `WorktreeCreate` · `WorktreeRemove` · `InstructionsLoaded` · `CwdChanged` · `FileChanged` · `DirectoryAdded` · `MessageDisplay` · `Notification`

## License

MIT
