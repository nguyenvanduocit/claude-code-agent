# claude-code-agent

Rust SDK for [Claude Code](https://claude.ai/claude-code). It drives the `claude` CLI over its stream-json control protocol. Verified against 2.1.329; `Session::spawn` refuses an older CLI (`MIN_CLI_VERSION`).

- one persistent `claude` process per session; `close()` ends the whole process group (process tree on Windows)
- every `claude --help` flag an SDK session can use, as a typed `SessionOptions` field (`extra_args` for the rest)
- every public control request as a `Session` method: model, effort, thinking, permission mode, interrupt, MCP servers, settings, usage, rewind, …
- a lossless event stream: text, thinking, tool calls and their results, subagent events, every `system` frame, the full turn result
- hook callbacks, `can_use_tool`, MCP elicitation and user-dialog handlers as async Rust closures
- Rust functions as tools (`#[claude_tool]` + SDK MCP bridge), executed in-process, cancellable
- Code Mode (`REPL` tool) forced on with `code_mode: true`
- text, image and document prompts (`UserMessage::blocks`)
- record/replay for tests without a process, session pool, `tracing` spans

## Install

```toml
claude-code-agent = { version = "0.6", features = ["macros"] }
```

## Quick start

```rust
use claude_code_agent::{Session, SessionOptions, Initialize, Agent, AgentEvent};
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
use claude_code_agent::{CanUseTool, HookRegistry, Initialize, Session, SessionOptions};
use claude_code_agent::protocol::{HookEventKind, HookOutput, ToolDecision};

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
use claude_code_agent::{SessionPool, PoolConfig, Agent, AgentEvent};

let pool = SessionPool::new(PoolConfig { max_sessions: 4, ..Default::default() });
pool.warm(2).await; // pre-warm 2 idle processes

let s = pool.acquire().await?; // waits while max_sessions are checked out
let mut stream = Agent::new(s.session.clone()).prompt("task one").await?;
// … drop `s` to return it; the next acquire() resets it with /clear first
```

Every pooled session uses `PoolConfig::options` (hooks, MCP servers, model). `pool.acquire_with(Checkout { model: Some("haiku".into()), ..Default::default() })` hands out a session with its own model, effort or permission mode; `None` keeps the pool's value. A returned session is reset before reuse: `/clear` (local, no API call, new session id) empties the conversation, and model, effort and permission mode are set for the next checkout. A task may switch them for itself without affecting the next task.

## Rust functions as tools — `#[claude_tool]` + SDK MCP server

```rust
use claude_code_agent::{claude_tool, SdkMcpServer, Session, SessionOptions, Initialize};

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
SDK tools are added to `--allowedTools` automatically. Enable with `claude-code-agent = { features = ["macros"] }`.

## Events and results

`Agent::prompt` streams one turn's `AgentEvent`s and ends with `Done(TurnResult)` or `Error(TurnError)`:

| Event | Carries |
|---|---|
| `Text`, `Thinking`, `RedactedThinking` | model output |
| `ToolUse { id, name, input, server }` | a tool call (`server: true` for API-run tools such as web search) |
| `ToolResult { tool_use_id, content, is_error, structured }` | the result; `structured` is the tool's full output, e.g. Bash `{stdout, stderr, …}` |
| `Subagent { parent_tool_use_id, event }` | any event of a subagent |
| `System { subtype, data }`, `Other { kind, data }` | every other frame, verbatim (`api_retry`, `task_started`, `rate_limit_event`, …) |
| `Done(TurnResult)` | `result` text, `structured_output`, cost, tokens, `num_turns`, `model_usage`, `permission_denials`, `terminal_reason` |
| `Error(TurnError)` | `kind` (`Execution`, `MaxTurns`, `MaxBudget`, `Api`, …), message, and the `TurnResult` so far |

Structured output: set `Initialize::json_schema`; the parsed value arrives in `TurnResult::structured_output` (`examples/structured_output.rs`).

`Agent::watch()` streams every turn, including the ones the CLI starts itself after a background task finishes (`TurnResult::origin` set). `examples/watch.rs`.

## Cancelling

Dropping a turn's stream before `Done`/`Error` cancels the prompt: a queued prompt is withdrawn, a running one is interrupted. Another prompt's turn is never touched. `Session::cancel_prompt(uuid)` does the same explicitly; `Session::interrupt()` stops whatever runs; `Session::interrupt_all()` also cancels every queued prompt. A prompt withdrawn before it ran ends its stream with `TurnErrorKind::Cancelled`. A prompt sent with `UserMessage::with_priority(Priority::Now)` aborts the running turn and runs at once; the aborted turn ends with `Done` and `terminal_reason: "aborted_streaming"`.

When the CLI withdraws a request it asked the host (e.g. `can_use_tool` during an interrupt), the handler's future is dropped and no answer is sent. An interrupted SDK tool call drops its handler the same way.

## MCP servers

- in-process: `SessionOptions::mcp_servers` (`SdkMcpServer`), with `timeout`, tool `title`/`annotations`/`output_schema`, and `ToolResult` text, image, resource and `structured` content. `SdkTool::with_context` gives the handler the call's `tool_use_id`.
- external (stdio, http, sse): `Initialize::mcp_servers`.
- at runtime: `Session::set_mcp_servers(sdk, external)`, `mcp_status`, `mcp_toggle`, `mcp_reconnect`, `mcp_call` (external servers, no model turn).
- elicitation: `SessionOptions::on_elicitation` answers an MCP server asking the user for input.

## Record / replay — tests without a process or API cost

```rust
let opts = SessionOptions { record: Some("hi.jsonl".into()), ..Default::default() };
// … run a live session once …
let session = claude_code_agent::replay::replay("hi.jsonl", SessionOptions::default()).await?;
```

## Compaction

```rust
let mut stream = claude_code_agent::compaction::compact(&agent, Some("keep file paths and errors")).await?;
// yields AgentEvent::Compacted { trigger: "manual", pre_tokens, post_tokens }, then Done
```

## Observability

`tracing` spans `claude.turn`, `claude.hook`, `claude.tool` — export with any subscriber (e.g. `tracing-opentelemetry`).

## Code Mode

```rust
let opts = SessionOptions { code_mode: true, ..Default::default() };
```

Experimental and unsupported by Anthropic: Code Mode is gated server-side, and this option forces it on. Claude gets the `REPL` tool and works by writing JavaScript that calls its tools. The stock CLI refuses Code Mode for every non-interactive session, so the SDK runs a patched copy. It is built on first use (about 6 s), cached under `~/.cache/claude-code-agent/code-mode/`, and re-signed on macOS. The installed `claude` is not modified. The patch changes only the Code Mode opt-in module: the eligibility check passes, the caveats count as accepted, and the server gate counts as open. See DESIGN.md.

## Session options worth knowing

| Option | Effect |
|---|---|
| `permission_mode: Some(PermissionMode::…)` | `--permission-mode` (default, acceptEdits, bypassPermissions, plan, dontAsk, auto) |
| `setting_sources: Some(vec![])` | ignore user/project settings (`--setting-sources ""`); auth still works |
| `settings: Some(json)` | extra settings (`--settings`): an object, or a file path string; e.g. shell hooks |
| `clear_env: true` | child gets only `env`, not the parent environment (default inherits, like the official SDKs) |
| `resume: Some(id)` / `Session::reconnect` | continue a previous conversation (`--resume`); with `fork_session: true` under a new id |
| `continue_conversation: true` | continue the latest conversation in `cwd` |
| `include_partial: true` | text arrives as stream deltas |
| `model`, `fallback_model`, `effort`, `thinking` | model choice; switch mid-session with `set_model`, `set_effort`, `set_max_thinking_tokens` |
| `max_turns`, `max_budget_usd`, `task_budget` | limits; a turn past one ends with `TurnErrorKind::MaxTurns` / `MaxBudget` |
| `tools`, `allowed_tools`, `disallowed_tools`, `add_dirs` | tool set and file access |
| `persist_session: false` | no transcript on disk |
| `on_stderr` | every CLI stderr line, live |
| `extra_args` | flags without a field |

## Keeping up with the CLI

`python3 -I scripts/cli-parity.py` lists every `claude --help` flag and public control request the SDK does not cover (exit 1 when any is left). Deliberate omissions, each with its reason, are in `scripts/cli-parity-ignore.txt`. Run it after each Claude Code update.

## Hook events

`HookEventKind` has all 33 event names in the 2.1.328 binary. Observed firing for SDK callbacks in a live run (`examples/hook_coverage.rs`): PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch, UserPromptSubmit, SessionStart, Stop, SubagentStart, SubagentStop, PreCompact, PostCompact, MessageDisplay. CwdChanged and FileChanged reach shell hooks in settings but not SDK callbacks. The others need situations that run did not produce (elicitation, worktrees, teammates, …).

`PreToolUse` · `PostToolUse` · `PostToolUseFailure` · `PostToolBatch` · `UserPromptSubmit` · `UserPromptExpansion` · `SessionStart` · `SessionEnd` · `Stop` · `StopFailure` · `SubagentStart` · `SubagentStop` · `PreCompact` · `PostCompact` · `PreModelSwitch` · `PostModelSwitch` · `PermissionRequest` · `PermissionDenied` · `Setup` · `TeammateIdle` · `TaskCreated` · `TaskCompleted` · `Elicitation` · `ElicitationResult` · `ConfigChange` · `WorktreeCreate` · `WorktreeRemove` · `InstructionsLoaded` · `CwdChanged` · `FileChanged` · `DirectoryAdded` · `MessageDisplay` · `Notification`

## User dialogs

Some CLI flows stop and ask the host app a question (`request_user_dialog`). Declare the kinds your app renders and answer from `on_user_dialog`; the CLI sends no other kind.

```rust
let opts = SessionOptions {
    on_user_dialog: Some(OnUserDialog::new(|d| async move {
        match d.dialog_kind.as_str() {
            "auto_mode_server_fallback" => UserDialogResponse::completed("continue"),
            _ => UserDialogResponse::cancelled(),
        }
    })),
    ..Default::default()
};
let init = Initialize { supported_dialog_kinds: Some(vec!["auto_mode_server_fallback".into()]), ..Default::default() };
```

CLI 2.1.329 routes these kinds to an SDK host. The handler receives every kind the same way; the table says what each answer does.

| Kind | When | Answers | Checked against the real CLI |
|---|---|---|---|
| `auto_mode_server_fallback` | auto mode falls back to billed classifier requests (e.g. API key behind a gateway) | `"continue"` runs the classifier; `"interrupt"` and `cancelled()` reject the tool call and end the turn | yes |
| `refusal_fallback_prompt` | a model refuses and a fallback model is set | `"retry_fallback"` retries on the fallback model; `"edit_prompt"` and `cancelled()` end the turn with the refusal | yes |
| `fable_overage_consent_prompt` | a subscription reaches its Fable usage limit | per payload; an answer can turn on overage billing | no: a real account below its limit gets no dialog |
| `send_to_project_remote_control_offer` | sending to another project needs Remote Control | `"turned_on"`, `"already_on"`, `"declined"`, `"failed"` | not sent to SDK sessions: 2.1.329 refuses `/send-to-project` there ("isn't available in this environment") |
| `enum_prompt` | routed, but no flow in 2.1.329 sends it | per payload | not sent by 2.1.329 |

`cancelled()` means the user dismissed the dialog; the CLI applies the dialog's default.

## Limitations

- `code_mode` runs a patched copy of the CLI. The patch anchors on minified code, so a CLI release that changes the Code Mode module makes spawn fail with `CodeModeError::Anchor` until the anchors are updated. Verified on 2.1.329.
- Tools of servers added with `set_mcp_servers` are not pre-allowed: the permission mode and `can_use_tool` decide.
- `mcp_call` and `mcp_read_resource` reach external MCP servers only; the CLI refuses in-process ones.
- `set_system_prompt` works only while the session runs the account's default model (CLI 2.1.329 cannot confirm another); `set_model_and_system_prompt` works always.
- `UserMessage::with_timestamp` is sent, but CLI 2.1.329 records its own receive time.
- No credential handling: the CLI uses its own login.

## License

MIT
