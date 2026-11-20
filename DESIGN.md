# claude-code-agent: design notes

> Source: the Claude Code binary (bundled JS chunks cited as `chunk-*.js:line` are from 2.1.328; the SDK is verified against 2.1.329), the official Python SDK (claude_agent_sdk 0.1.48), and live runs.
> Claims carry a citation and a confidence label (HIGH/MEDIUM/LOW).

---

## Why a New SDK

Existing community Rust SDKs (e.g. `claude-rs`, `anthropic-sdk-rust`) expose 4–6 message types and spawn/kill a child process per request. Claude Code's internal TypeScript SDK (`chunk-f89nsf5q.query.js`) uses a **persistent subprocess** and the stream-json control protocol (hooks, permissions, in-process MCP, control requests). This SDK brings that model to Rust.

---

## Transport: Persistent Subprocess (HIGH)

Source: `chunk-f89nsf5q.query.js:614, 915–949`

```
claude --output-format stream-json \
       --verbose \
       --input-format stream-json \
       --await-initialize \
       --replay-user-messages
```

- One long-lived process per session. Stdin/stdout are newline-delimited JSON.
- Send a query by writing `{"type":"user","message":{"role":"user","content":[...]},"uuid":"…"}` + `\n`; the `uuid` marks the turn (see Turn boundaries).
- `--await-initialize` makes the CLI wait for the `initialize` control request before anything else.
- `Session` is `Arc<SessionInner>` with three tasks: writer (stdin), reader (stdout → subscribers, answers control requests), watchdog (exit + stderr tail). See Lifecycle.

---

## Protocol Messages (HIGH)

Source: `chunk-20bv8pca.js` (Zod schema file, 20 000+ lines)

### Outbound (host → CLI)

| Type | Schema location | Description |
|---|---|---|
| `initialize` | line 17716–17891 `qfo` | Session init; 30+ fields |
| `user` | — | A turn prompt (`message`, `uuid`, `parent_tool_use_id`) |
| `control_response` to `hook_callback` | line 19959–19979 | Hook output (approve/block/context) |
| `control_response` to `can_use_tool` | — | Allow (with `updatedInput`) or deny a tool call |
| `background_tasks` control | line 20970–20995 | Programmatically background an agent |
| `interrupt`, `set_model`, `set_permission_mode`, `stop_task`, `get_context_usage` controls | binary control schemas | `Session` methods |

### Inbound (CLI → host)

Top-level `type`s (`protocol::InboundMessage`):

| Type | Notes |
|---|---|
| `system` | `subtype` selects the frame, see below |
| `user` | Prompt echo (`isReplay: true`, carries the prompt's `uuid`) or tool results |
| `assistant` | Complete message; content blocks `text`, `thinking`, `tool_use` |
| `stream_event` | Partial content (needs `--include-partial-messages`) |
| `result` | Turn complete: `success`, or `error_*` (line 14425–14913) |
| `control_request` | CLI asks the host: `hook_callback` (line 19959–19979), `can_use_tool`, `mcp_message` |
| `control_response` | Answer to a host control request (initialize, interrupt, …) |
| `keep_alive` | Process keepalive |

`system` subtypes:

| Subtype | Schema location | Notes |
|---|---|---|
| `init` | `chunk-hp1k7mph.js:202` | Per-turn metadata (tools, model, …) |
| `background_tasks_changed` | line 16351 | Level signal (replace semantics) |
| `thinking_tokens` | line 16902 | Extended thinking partial |
| `compact_boundary` | line 15408 | Context-compaction checkpoint |
| `model_fallback` | line 15898 | Model switched |
| `model_refusal_no_fallback` | line 15892 | Model refused, no fallback |
| `peer_message_hold` | line 15972 | Multi-session coordination |
| `task_started`, `task_updated`, `task_notification`, `hook_started`, `hook_response` | — | Task and hook progress |

---

## Initialize Message (HIGH)

Source: initialize zod schema in the 2.1.328 binary; claude_agent_sdk 0.1.48 `_internal/query.py:128–160`.

The first stdin line (required by `--await-initialize`) is a control request:

```json
{"type":"control_request","request_id":"req_0_…","request":{
  "subtype":"initialize",
  "hooks":{"PreToolUse":[{"matcher":"Bash","hookCallbackIds":["hook_0"],"timeout":30}]},
  "systemPrompt":"…","appendSystemPrompt":"…","agents":{…},"jsonSchema":{…}}}
```

The CLI answers `{"type":"control_response","response":{"subtype":"success","request_id":"req_0_…","response":{commands, models, account, …}}}`.
`system/init` arrives only with the first user turn.

Other top-level fields in the schema: `sdkMcpServers`, `sdkMcpServerConfigs`, `sdkMcpServerManifests`, `systemPromptSnapshot`, `planModeInstructions`, `appendSubagentSystemPrompt`, `toolAliases`, `codeRunningMcpTools`, `excludeDynamicSections`, `title`, `skills`, `promptSuggestions`, `agentProgressSummaries`, `forwardSubagentText`, `supportedDialogKinds`.
cwd, model, permission mode and resume are process-level: cwd of the child, `--model`, `--permission-mode`, `--resume`.

---

## 33 Hook Types (HIGH)

Source: hook event array in the 2.1.328 binary (diffed 33/33 against `HookEventKind`).

```
PreToolUse  PostToolUse  PostToolUseFailure  PostToolBatch  Notification
UserPromptSubmit  UserPromptExpansion  SessionStart  SessionEnd  Stop  StopFailure
SubagentStart  SubagentStop  PreCompact  PostCompact  PreModelSwitch  PostModelSwitch
PermissionRequest  PermissionDenied  Setup  TeammateIdle  TaskCreated  TaskCompleted
Elicitation  ElicitationResult  ConfigChange  WorktreeCreate  WorktreeRemove
InstructionsLoaded  CwdChanged  FileChanged  DirectoryAdded  MessageDisplay
```

### Hook Timeouts (HIGH)

Source: `chunk-0f728242.js:432–448`

| Hook(s) | Timeout |
|---|---|
| `PreToolUse` / `PostToolUse` / `PostToolUseFailure` / `PostToolBatch` | 15 s |
| `UserPromptSubmit` / `UserPromptExpansion` | 30 s |
| `Stop` / `StopFailure` / `SubagentStop` | 120 s |
| All others | not listed in that source |

### Hook Callback IPC (HIGH)

Source: binary hook_callback + hook-output schemas; `query.py:285–300, :325–345`. Verified end-to-end by `tests/integration_test.rs`.

CLI → host: `{"type":"control_request","request_id":"…","request":{"subtype":"hook_callback","callback_id":"hook_0","input":{hook_event_name, tool_name, tool_input, …},"tool_use_id":"…"}}`

Host → CLI: `{"type":"control_response","response":{"subtype":"success","request_id":"…","response":<hook output>}}`, where the hook output is the shell-hook JSON:
`continue`, `suppressOutput`, `stopReason`, `decision` (`approve`|`block`), `systemMessage`, `reason`, `hookSpecificOutput` (`{hookEventName, additionalContext | watchPaths | …}`).

`can_use_tool` (only with `--permission-prompt-tool stdio`) uses the same envelope; the answer is `{"behavior":"allow","updatedInput":{…}}` or `{"behavior":"deny","message":"…","interrupt"?}`.

---

## Code Mode (forced on through a patched CLI copy)

The stock CLI (verified 2.1.329) refuses Code Mode for every non-interactive session.
Its opt-in check returns `"entrypoint"` when `!isInteractive()` or
`CLAUDE_CODE_ENTRYPOINT !== "cli"`, so `-p` and stream-json sessions (every SDK)
never get the `REPL` tool. The check also refuses:

- the env var set only in the shell (`own_settings`); it must also come from settings
- models older than Opus/Sonnet 5.5 (Fable/Mythos 5.2)
- non-first-party providers, OTEL tracing, HIPAA
- the `tengu_rustling_blossom` server gate being closed
- the one-time caveats dialog not yet accepted (`codeModeCaveatsAcceptedV1`)

`SessionOptions::code_mode = true` (`src/code_mode.rs`) runs a patched copy instead:

1. Find the opt-in module in Bun's embedded module graph: it is the module whose
   source contains `"tengu_rustling_blossom"`.
2. Make same-length edits to its source. The eligibility check returns early, the
   caveats count as accepted, and the gate counts as open. Each anchor is a regex
   over minified names and must match exactly once, otherwise spawning fails with
   `CodeModeError::Anchor`.
3. Zero the module's bytecode pointer. Bun runs the precompiled JSC bytecode when
   it is present, and the bytecode ignores source edits (verified). With the
   pointer zeroed, Bun compiles the patched source.
4. On macOS, re-sign ad hoc with the stock entitlements (JIT). Without them the
   modified binary is killed.
5. Cache the copy in `~/.cache/claude-code-agent/code-mode/<name>-<size>-<mtime>-sdk<version>/claude`
   (`$XDG_CACHE_HOME` when set). The first spawn takes about 6 s; later spawns reuse
   the copy, and a CLI update or a new SDK version builds a new one. Each build
   records its source path; a build whose source CLI changed or is gone is deleted,
   so builds for other installed CLIs stay.

The SDK also passes `CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1` in the process env and in
`--settings` `env`. Verified live: `REPL` appears in `system/init.tools` and Claude
calls it (`tests/integration_test.rs::code_mode_exposes_repl`, `examples/code_mode.rs`).

---

## Workflow / Background Tasks (HIGH)

Source: `chunk-20bv8pca.js:16351–16369` (`M6` schema, `background_tasks_changed`), `chunk-20bv8pca.js:16307–16311` (`task_type`, `workflow_name` fields), `chunk-20bv8pca.js:20970–20978` (`fl` schema, `background_tasks` control request)

**`background_tasks_changed`** is a **level signal** (replace semantics — entire list replaces prior). Fields include `parent_task_id` for the full agent tree.

**`background_tasks` control request** (`chunk-20bv8pca.js:20970–20978`): with `tool_use_id` → backgrounds one specific task; without it → backgrounds all foreground tasks (Ctrl+B semantics). The description says: *"Each blocking tool call returns immediately with a 'running in the background' tool_result and the turn continues."*

**Workflow task type**: `task_type: "local_workflow"` in `background_tasks_changed` events (`chunk-20bv8pca.js:16307–16311`). `workflow_name` carries `meta.name` from the workflow script.

`protocol::BackgroundTask` fields: `task_id`, `run_id`, `task_type` (e.g. `"local_bash"`, `"local_workflow"`), `subagent_type`, `description`, `parent_task_id`, `ambient`, `awaited`, `off_host`. `background::TaskTree` builds the tree from `parent_task_id`.

---

## Tool Search (HIGH)

Source: search for `ENABLE_TOOL_SEARCH` in source; beta `tool-search-tool-2025-10-19` appears in `system/init.betas[]`.

Set `ENABLE_TOOL_SEARCH=1` in subprocess env to activate. Distinct from Code Mode.

---

## Architecture

```
claude-code-agent/
├── src/
│   ├── lib.rs
│   ├── code_mode.rs     # patched CLI copy with Code Mode forced on, cached
│   ├── protocol.rs      # wire types (control envelopes, frames, hook I/O)
│   ├── session.rs       # process transport + reader/writer/watchdog, answers control requests
│   ├── agent.rs         # one prompt → AgentEvent stream, tracing span per turn
│   ├── hooks.rs         # HookRegistry: async closures → initialize `hooks` + hook_callback
│   ├── mcp.rs           # in-process MCP servers (SdkMcpServer, SdkTool) via mcp_message
│   ├── pool.rs          # SessionPool: pre-warmed sessions, idle reuse
│   ├── replay.rs        # record stdout to JSONL, replay without a process
│   ├── compaction.rs    # /compact with custom instructions
│   ├── background.rs    # TaskTree over background_tasks_changed snapshots
│   ├── fs_events.rs     # FsEvent decoded from FileChanged/CwdChanged/DirectoryAdded
│   └── middleware.rs    # ready-made hook policies (ToolAllowList)
├── claude-code-agent-macros/   # #[claude_tool] → <fn>_tool_def() + <fn>_tool()
├── scripts/             # cli-parity.py: SDK coverage of the installed CLI's flags and requests
└── tests/               # integration_test.rs (live CLI, --ignored), replay_test.rs, macro_usage.rs,
                         # process_tree.rs (close() ends the process tree; runs on every CI OS)
```

## Observability

The SDK emits `tracing` spans: `claude.turn` (cost_usd, input/output tokens, ttft_ms, error),
`claude.hook` (callback_id, event, tool, blocked), `claude.tool` (server, method, tool).
Export them with any subscriber, e.g. `tracing-opentelemetry` for OTLP.

## Frame shapes (verified against the 2.1.328 binary and live output)

`compact_boundary`, `model_fallback`, `background_tasks_changed`, `task_started`,
`task_updated`, `task_notification`, `thinking_tokens`, `model_refusal_no_fallback`
and `peer_message_hold` are all `{"type":"system","subtype":…}` frames.
A `result` with an `origin` field (e.g. `{"kind":"task-notification"}`) closes a turn
the CLI started itself; prompted turns have no `origin`.

## Turn boundaries

The CLI runs turns one after another; each ends with a `result`. The session runs with
`--replay-user-messages`, and every prompt carries a `uuid`, which the CLI echoes
(`isReplay: true`) inside that prompt's turn — first for a normal prompt, last for a
slash command (`/compact` echoes after `compact_boundary`). A prompt's stream therefore
holds frames since the previous `result` until its own echo arrives; a `result` before
the echo closes another turn (a queued prompt, or one the CLI started itself after a
background task finished) and the held frames are dropped.

A prompt the CLI refuses before running it (for example over `max_budget_usd`) gets a
`result` but no echo; that `result` lists the prompt in `user_message_uuids`, which also
ends its stream. A prompt cancelled while queued gets neither: the CLI sends
`command_lifecycle` with state `cancelled`, and the stream ends with
`TurnErrorKind::Cancelled`. Dropping a stream before its end cancels its prompt
(`cancel_async_message` if queued, otherwise `interrupt` for that prompt only).

## Lifecycle

The CLI runs in its own process group (`process_group(0)`), so tools it starts (Bash,
background tasks) are in that group too. The Session owns the only strong stdin
sender. `close()` closes stdin and waits up to 5 s for the CLI to exit; a CLI kept
alive by background tasks then gets SIGTERM, then SIGKILL, sent to the whole group,
and a final SIGTERM to the group ends leftover children. Dropping the last Session
clone sends SIGTERM to the group if the CLI is still running. On Windows the same
steps use `taskkill /T /F` on the CLI's process tree.

## Event fanout

The reader task sends each frame to every subscriber through its own unbounded
channel. No frame is dropped for a slow consumer: a lost echo or `result` would hang
the turn. When stdout ends, all subscriber channels close and in-flight streams end
with `Error`. Assistant frames with `parent_tool_use_id` belong to a subagent and are
not turned into the parent turn's `AgentEvent`s.

## Tool aliasing


`chunk-hp1k7mph.js:69–70`: `gt = "Agent"`, `sf = "Task"` (`chunk-6r0zxzh1.js:3,15`). The init frame lists the tool `"Agent"` as `"Task"`. Custom aliases are set with `Initialize::tool_aliases`.

---

## Protocol notes (not implemented by the SDK)

Found in the binary; the SDK does not use them yet.

### `sdkMcpServerManifests` (HIGH)

Source: `chunk-20bv8pca.js:17732–17752` (schema + docstring), `chunk-20bv8pca.js:18054` (response field `sdk_mcp_manifests_parked`)

Pre-load MCP server handshake results in the `initialize` message to skip N×2 round-trips at startup. The docstring at 17747 says: *"MCP handshake results the host already obtained from its in-process servers by delivering initialize + notifications/initialized (+ tools/list) to them itself before writing this request. For each such server the CLI answers its own MCP client's initialize and first tools/list from these results and skips the notifications/initialized round trip."*

Response field `sdk_mcp_manifests_parked` reports per-entry outcome: `"parked"` | `"already_connected"` | `"protocol_version_mismatch"` | `"malformed"` | `"not_honoured"` (`chunk-20bv8pca.js:18054`).


### Semantic compaction via `PreCompact` (LOW: not verified)

Source: `chunk-20bv8pca.js` hook list; `compact_boundary` schema line 15408

When context nears the limit, the CLI fires a `PreCompact` hook before compacting. A host might steer or replace the summary from that hook; this is unverified. `compaction::compact` covers the verified path: `/compact <instructions>`.

### Device hook registration (HIGH)

Source: `chunk-zhqawkqb.createDeviceHooksWorker.js`

Protocol: `register_device_hooks` with `instance_id`, `hooks[]`, `templates[]`, `worker_epoch`.
Lease: 300 s. Response: `status`, `lease_s`, `lease_expires_at`, `accepted`, `ignored`, `ignored_ids`, `replaced_owner`.

## Open Items

1. **Code Mode patch anchors** are regexes over minified code; a CLI release that reshapes the opt-in module needs new anchors (spawn fails loudly until then).

