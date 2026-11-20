# claude-sdk

Rust SDK for [Claude Code](https://claude.ai/claude-code). It drives the `claude` CLI (verified against 2.1.328) over its stream-json control protocol:

- one persistent `claude` process per session; `close()` ends the whole process group
- hook callbacks and `can_use_tool` permission handlers as async Rust closures
- Rust functions as tools (`#[claude_tool]` + SDK MCP bridge), executed in-process
- control requests: `interrupt`, `set_model`, `set_permission_mode`, `stop_task`, `context_usage`
- Code Mode (`REPL` tool) forced on with `code_mode: true`
- text and image prompts (`UserMessage::blocks` + `ContentBlock::image_base64`)
- record/replay for tests without a process, session pool, `tracing` spans

## Install

Pin the git dependency to a commit or tag:

```toml
claude-sdk = { git = "https://github.com/nguyenvanduocit/claude-sdk-rs", tag = "v0.3.0", features = ["macros"] }
```

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

## Hooks — async Rust closures

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
pool.warm(2).await; // pre-warm 2 idle processes

let s = pool.acquire().await?; // waits while max_sessions are checked out
let mut stream = Agent::new(s.session.clone()).prompt("task one").await?;
// … drop `s` to return it; the next acquire() resets it with /clear first
```

Every pooled session uses `PoolConfig::options` (hooks, MCP servers, model). A returned session is reset with `/clear` (local, no API call, new session id) before reuse, so tasks never see each other's conversation.

## Rust functions as tools — `#[claude_tool]` + SDK MCP server

```rust
use claude_sdk::{claude_tool, SdkMcpServer, Session, SessionOptions, Initialize};

/// Search files matching a glob pattern
#[claude_tool]
async fn search_files(pattern: String, max_results: u64) -> anyhow::Result<Vec<String>> {
    let found = glob::glob(&pattern)?.flatten().take(max_results as usize);
    Ok(found.map(|p| p.display().to_string()).collect())
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

## Code Mode

```rust
let opts = SessionOptions { code_mode: true, ..Default::default() };
```

Experimental and unsupported by Anthropic: Code Mode is gated server-side, and this option forces it on. Claude gets the `REPL` tool and works by writing JavaScript that calls its tools. The stock CLI refuses Code Mode for every non-interactive session, so the SDK runs a patched copy. It is built on first use (about 6 s), cached under `~/.cache/claude-sdk/code-mode/`, and re-signed on macOS. The installed `claude` is not modified. The patch changes only the Code Mode opt-in module: the eligibility check passes, the caveats count as accepted, and the server gate counts as open. See DESIGN.md.

## Session options worth knowing

| Option | Effect |
|---|---|
| `permission_mode: Some(PermissionMode::…)` | `--permission-mode` (default, acceptEdits, bypassPermissions, plan, dontAsk, auto) |
| `setting_sources: Some(vec![])` | ignore user/project settings (`--setting-sources ""`); auth still works |
| `settings: Some(json)` | extra settings (`--settings`): an object, or a file path string; e.g. shell hooks |
| `clear_env: true` | child gets only `env`, not the parent environment (default inherits, like the official SDKs) |
| `resume: Some(id)` / `Session::reconnect` | continue a previous conversation (`--resume`) |
| `include_partial: true` | text arrives as stream deltas |

## Hook events

`HookEventKind` has all 33 event names in the 2.1.328 binary. Observed firing for SDK callbacks in a live run (`examples/hook_coverage.rs`): PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch, UserPromptSubmit, SessionStart, Stop, SubagentStart, SubagentStop, PreCompact, PostCompact, MessageDisplay. CwdChanged and FileChanged reach shell hooks in settings but not SDK callbacks. The others need situations that run did not produce (elicitation, worktrees, teammates, …).

`PreToolUse` · `PostToolUse` · `PostToolUseFailure` · `PostToolBatch` · `UserPromptSubmit` · `UserPromptExpansion` · `SessionStart` · `SessionEnd` · `Stop` · `StopFailure` · `SubagentStart` · `SubagentStop` · `PreCompact` · `PostCompact` · `PreModelSwitch` · `PostModelSwitch` · `PermissionRequest` · `PermissionDenied` · `Setup` · `TeammateIdle` · `TaskCreated` · `TaskCompleted` · `Elicitation` · `ElicitationResult` · `ConfigChange` · `WorktreeCreate` · `WorktreeRemove` · `InstructionsLoaded` · `CwdChanged` · `FileChanged` · `DirectoryAdded` · `MessageDisplay` · `Notification`

## Limitations

- `code_mode` runs a patched copy of the CLI. The patch anchors on minified code, so a CLI release that changes the Code Mode module makes spawn fail with `CodeModeError::Anchor` until the anchors are updated. Verified on 2.1.329.
- Tool results from SDK tools are text.
- No credential handling: the CLI uses its own login.

## License

MIT
