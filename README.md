# claude-code-agent

Run [Claude Code](https://claude.ai/claude-code) from Rust. Each `Session` keeps one `claude` process running, so you can send prompts, watch Claude work, approve or block its tools, and give it tools written in Rust.

You need the Claude Code CLI 2.1.329 or newer, installed and logged in. `Session::spawn` refuses an older version.

## Install

```toml
[dependencies]
claude-code-agent = "0.7"
tokio = { version = "1", features = ["full"] }
tokio-stream = "0.1"
```

Add `features = ["macros"]` to turn Rust functions into tools with `#[claude_tool]`.

## Quick start

```rust
use claude_code_agent::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent.prompt("What files are in this directory?").await?;
    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text(text) => print!("{text}"),
            AgentEvent::Done(result) => {
                println!("\ncost: ${:.4}", result.total_cost_usd.unwrap_or(0.0));
                break;
            }
            AgentEvent::Error(error) => anyhow::bail!(error),
            _ => {}
        }
    }
    session.close().await;
    Ok(())
}
```

`close()` ends the `claude` process and everything it started.

## Choosing the model and limits

Set these in `SessionOptions` before spawning:

| Option | What it does |
|---|---|
| `model`, `fallback_model` | Which model to use, and which to fall back to |
| `effort`, `thinking` | How hard the model thinks |
| `permission_mode` | `Default`, `AcceptEdits`, `BypassPermissions`, `Plan`, `DontAsk` or `Auto` |
| `max_turns`, `max_budget_usd` | Stop a prompt after this many turns or this much spend |
| `tools`, `allowed_tools`, `disallowed_tools`, `add_dirs` | Which tools Claude has and which folders it may use |
| `resume`, `fork_session`, `continue_conversation` | Continue an earlier conversation |
| `cwd`, `env`, `settings`, `setting_sources` | Working folder, environment and Claude Code settings |
| `persist_session: false` | Don't save the conversation to disk |
| `include_partial: true` | Receive text as it is typed, in small pieces |
| `extra_args` | Any other `claude` flag |

Change them mid-session with `set_model`, `set_effort` and `set_permission_mode`.

## What a prompt sends back

`agent.prompt(...)` returns a stream of `AgentEvent`s for that prompt:

| Event | Meaning |
|---|---|
| `Text`, `Thinking` | What Claude writes and thinks |
| `ToolUse` | Claude calls a tool |
| `ToolResult` | The tool's result; `structured` holds the full output, such as Bash's stdout and stderr |
| `Subagent` | An event from a subagent Claude started |
| `System`, `Other` | Everything else the CLI reports, such as retries and rate limits |
| `Done(TurnResult)` | Finished: the answer, cost, tokens and number of turns |
| `Error(TurnError)` | Failed; `kind` says why: `MaxTurns`, `MaxBudget`, `Api`, `Cancelled`, … |

To get the answer as JSON, set `Initialize::json_schema`. The value arrives in `TurnResult::structured_output` (see `examples/structured_output.rs`).

Claude sometimes starts a new turn by itself, for example when a background command finishes. `agent.watch()` streams every turn, including those (see `examples/watch.rs`).

## Stopping a prompt

Drop the stream to cancel its prompt. A prompt still waiting in line is removed; a running one is stopped. Other prompts keep going.

- `session.interrupt()` stops whatever is running.
- `session.interrupt_all()` also removes every waiting prompt.
- `UserMessage::with_priority(Priority::Now)` stops the running prompt and starts yours right away.

## Approving tools and hooks

Decide which tools Claude may use with `can_use_tool`, and react to what Claude does with hooks. Both are set before spawning:

```rust
use claude_code_agent::protocol::{HookEventKind, HookOutput, ToolDecision};
use claude_code_agent::{CanUseTool, HookRegistry, SessionOptions};

let mut hooks = HookRegistry::new();

// Block every Bash command
hooks.on(HookEventKind::PreToolUse, Some("Bash"), |_| async move {
    HookOutput::block("Bash is disabled")
});

// Give every subagent extra instructions
hooks.on(HookEventKind::SubagentStart, None, |_| async move {
    HookOutput::with_context(HookEventKind::SubagentStart, "Use at most 5 tool calls.")
});

let opts = SessionOptions {
    hooks,
    // Called whenever Claude Code would ask for permission
    can_use_tool: Some(CanUseTool::new(|req| async move {
        if req.tool_name == "Read" {
            ToolDecision::allow()
        } else {
            ToolDecision::deny(format!("{} is not allowed", req.tool_name))
        }
    })),
    ..Default::default()
};
```

`HookEventKind` has every Claude Code hook. These fire for Rust hooks: `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `UserPromptSubmit`, `SessionStart`, `Stop`, `SubagentStart`, `SubagentStop`, `PreCompact`, `PostCompact` and `MessageDisplay`. `CwdChanged` and `FileChanged` only reach shell hooks in your settings.

## Tools written in Rust

With the `macros` feature, a Rust function becomes a tool Claude can call. It runs inside your program:

```rust
use claude_code_agent::{claude_tool, SdkMcpServer, SessionOptions};

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
```

Claude sees the tool as `mcp__files__search_files`. The doc comment becomes its description, and the arguments are read from the call with serde. An `Err` is shown to Claude as a failed call. Your tools are allowed to run without asking.

Other MCP servers (stdio, HTTP or SSE) go in `Initialize::mcp_servers`. Add or change servers mid-session with `session.set_mcp_servers(...)`.

## Questions from the CLI

Sometimes Claude Code needs a decision from your user before it can go on. Without a handler it doesn't ask and picks a safe default. To ask your user yourself, list the questions your app can show and answer them in `on_user_dialog`:

```rust
use claude_code_agent::{Initialize, OnUserDialog, SessionOptions, UserDialogResponse};

let opts = SessionOptions {
    fallback_model: Some("claude-opus-4-8".into()),
    on_user_dialog: Some(OnUserDialog::new(|question| async move {
        match question.dialog_kind.as_str() {
            // Ask your user here; this example always retries
            "refusal_fallback_prompt" => UserDialogResponse::completed("retry_fallback"),
            _ => UserDialogResponse::cancelled(),
        }
    })),
    ..Default::default()
};
let init = Initialize {
    supported_dialog_kinds: Some(vec!["refusal_fallback_prompt".into()]),
    ..Default::default()
};
```

| Question | When Claude Code asks | Answers | With no handler |
|---|---|---|---|
| `refusal_fallback_prompt` | The model refused and `fallback_model` is set | `"retry_fallback"` tries the fallback model; `"edit_prompt"` stops | The prompt ends with the refusal |
| `auto_mode_server_fallback` | In auto mode, checking tool calls starts to cost extra API requests | `"continue"` pays and goes on; `"interrupt"` blocks the tool call | The checks run and are billed |
| `fable_overage_consent_prompt` | Your plan reached its Fable usage limit | See `question.payload`; an answer can turn on overage billing | Not asked |

`UserDialogResponse::cancelled()` means your user closed the question without choosing.

## Running many prompts at once

A `SessionPool` keeps `claude` processes warm and hands them out:

```rust
use claude_code_agent::{Agent, Checkout, PoolConfig, SessionPool};

let pool = SessionPool::new(PoolConfig { max_sessions: 4, ..Default::default() });
pool.warm(2).await; // start 2 processes now

let session = pool.acquire().await?; // waits while all 4 are in use
let mut stream = Agent::new(session.session.clone()).prompt("task one").await?;

// A session with its own model
let fast = pool.acquire_with(Checkout { model: Some("haiku".into()), ..Default::default() }).await?;
```

Dropping a pooled session returns it. Before the next task gets it, its conversation is cleared and its model, effort and permission mode are reset.

## Testing without the CLI

Record a live session once, then replay it in tests with no process and no API cost:

```rust
let opts = SessionOptions { record: Some("hi.jsonl".into()), ..Default::default() };
// ... run the session ...
let session = claude_code_agent::replay::replay("hi.jsonl", SessionOptions::default()).await?;
```

## More

- **Compacting:** `claude_code_agent::compaction::compact(&agent, Some("keep file paths"))` shrinks the conversation to save context.
- **Images and documents:** send them as prompt content with `UserMessage::blocks`.
- **Tracing:** spans `claude.turn`, `claude.hook` and `claude.tool` work with any `tracing` subscriber.
- **CLI output:** `on_stderr` receives every line the CLI writes to stderr.
- **Code Mode:** `code_mode: true` gives Claude a `REPL` tool where it writes JavaScript that calls its tools. Anthropic doesn't support this; the CLI only allows it interactively, so the SDK runs a patched copy of the CLI (built on first use, cached in `~/.cache/claude-code-agent/`). Your installed `claude` is not changed. See `DESIGN.md`.

Runnable programs for each feature are in `examples/`.

## Limitations

- A Claude Code update can break Code Mode until the SDK is updated; spawning then fails with a `CodeModeError` (`AnchorCount`, `PatternInvalid` or `ReplacementTooShort`). A CLI older than the SDK supports fails with `SessionError::UnsupportedCliVersion` instead.
- `close()` and dropping a session end the CLI, its hooks and its Bash tools. A process a hook or tool detached that outlived it (`(cmd &)`, `nohup cmd &`) is out of reach.
- Tools from servers added with `set_mcp_servers` still need permission (`permission_mode` or `can_use_tool`).
- `session.mcp_call` and `mcp_read_resource` work only with external MCP servers.
- `set_system_prompt` works only on your account's default model; use `set_model_and_system_prompt` otherwise.
- `UserMessage::with_timestamp` is ignored by Claude Code 2.1.329.
- The SDK doesn't handle login; the CLI uses its own.

## Development

Release: bump `version` in both `Cargo.toml` files (and the dependency on `claude-code-agent-macros`), push to `main`, then push the matching tag:

```sh
git tag -a v0.7.1 -m v0.7.1 && git push origin v0.7.1
```

`.github/workflows/release.yml` runs the tests on Linux, macOS and Windows, then publishes both crates to crates.io. No token is stored; crates.io trusts the workflow directly.

After a Claude Code update, run `python3 -I scripts/cli-parity.py` to list new CLI flags and requests the SDK doesn't support yet. Deliberate gaps and their reasons are in `scripts/cli-parity-ignore.txt`.

To read the CLI's own source, run `python3 -I scripts/decompile-claude.py`. It unpacks the installed `claude` binary into `target/claude-src/<version>/`; `DESIGN.md` cites those files as `chunk-*.js:line`.

## License

MIT
