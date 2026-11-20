# claude-sdk-rs — Design Document

> Source: decompiled Claude Code 2.1.328 (`learnSecurity/hackingtool/claude/readsrc/out/2.1.328/src/`).
> Every claim carries a `file:line` citation and a confidence label (HIGH/MEDIUM/LOW).

---

## Why a New SDK

Existing community Rust SDKs (e.g. `claude-rs`, `anthropic-sdk-rust`) expose 4–6 message types and spawn/kill a child process per request. Claude Code's internal TypeScript SDK (`chunk-f89nsf5q.query.js`) uses a **persistent subprocess** and exposes the full 30+ message protocol. This SDK closes that gap.

---

## Transport: Persistent Subprocess (HIGH)

Source: `chunk-f89nsf5q.query.js:614, 915–949`

```
claude --output-format stream-json \
       --verbose \
       --input-format stream-json \
       --await-initialize
```

- One long-lived process per session. Stdin/stdout are newline-delimited JSON.
- Send a query by writing `{"type":"user","message":{"role":"user","content":[...]}}` + `\n`.
- Read response lines until a `result/*` frame arrives.
- End by calling `.processStdin.end()` — do NOT kill the process mid-turn.
- `--await-initialize` makes the subprocess emit `{"type":"system","subtype":"init"}` before accepting queries.

```rust
pub struct Session {
    child:  Child,
    stdin:  BufWriter<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
}
```

---

## Protocol Messages (HIGH)

Source: `chunk-20bv8pca.js` (Zod schema file, 20 000+ lines)

### Outbound (host → CLI)

| Type | Schema location | Description |
|---|---|---|
| `initialize` | line 17716–17891 `qfo` | Session init; 30+ fields |
| `user_message` | — | A turn prompt |
| `hook_callback` control | line 19959–19979 | Approve/deny/modify hook event |
| `can_use_tool` control response | — | Approve/deny a tool call |
| `background_tasks` control | line 20970–20995 | Programmatically background an agent |

### Inbound (CLI → host)

| Type | Subtype | Schema location | Notes |
|---|---|---|---|
| `system` | `init` | `chunk-hp1k7mph.js:202` | Per-turn metadata frame |
| `assistant` | — | — | Model turn text |
| `tool_use` | — | — | Tool call request |
| `tool_result` | — | — | Tool call result |
| `result` | `success` | line 14425–14913 | Turn complete, usage/timing |
| `result` | `error_*` | — | Turn failed |
| `hook_callback` | — | line 19959–19979 | Hook event needing approval |
| `background_tasks_changed` | — | line 16351 | Level signal (replace semantics) |
| `thinking_tokens` | — | line 16902 | Extended thinking partial |
| `compact_boundary` | — | line 15408 | Context-compaction checkpoint |
| `model_fallback` | — | line 15898 | Model switched |
| `model_refusal_no_fallback` | — | line 15892 | Model refused, no fallback |
| `peer_message_hold` | — | line 15972 | Multi-session coordination |
| `stream_event` | — | — | Partial content (needs `--include-partial-messages`) |

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
| All others | default 5 s |

### Hook Callback IPC (HIGH)

Source: binary hook_callback + hook-output schemas; `query.py:285–300, :325–345`. Verified end-to-end by `tests/integration_test.rs`.

CLI → host: `{"type":"control_request","request_id":"…","request":{"subtype":"hook_callback","callback_id":"hook_0","input":{hook_event_name, tool_name, tool_input, …},"tool_use_id":"…"}}`

Host → CLI: `{"type":"control_response","response":{"subtype":"success","request_id":"…","response":<hook output>}}`, where the hook output is the shell-hook JSON:
`continue`, `suppressOutput`, `stopReason`, `decision` (`approve`|`block`), `systemMessage`, `reason`, `hookSpecificOutput` (`{hookEventName, additionalContext | watchPaths | …}`).

`can_use_tool` (only with `--permission-prompt-tool stdio`) uses the same envelope; the answer is `{"behavior":"allow","updatedInput":{…}}` or `{"behavior":"deny","message":"…","interrupt"?}`.

### Device Hook Registration (HIGH)

Source: `chunk-zhqawkqb.createDeviceHooksWorker.js`

Protocol: `register_device_hooks` with `instance_id`, `hooks[]`, `templates[]`, `worker_epoch`.
Lease: 300 s. Response: `status`, `lease_s`, `lease_expires_at`, `accepted`, `ignored`, `ignored_ids`, `replaced_owner`.

---

## Code Mode (HIGH env var, MEDIUM tool detection)

Source:
- `chunk-0jv1f6sm.codeModeShape.js:13–15` — env vars
- `chunk-kbdgp1g1.js:1–10` — tool names
- `chunk-56bhcxay.js:11259` — session flag

**Activation**: set `CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1` in the subprocess env. Code Mode is **not** a `betas[]` entry — it is an internal session state flag (`codeMode` in `Fsr` array, `chunk-56bhcxay.js:11259`).

**Tool names** (`chunk-kbdgp1g1.js:3–7`):
```js
var Ko = "REPL",   // the REPL execution tool
    mX = "Eval",   // the Eval tool
    CZt = "Code";  // internal name
function ga(o) { return o === Ko || o === mX; }
```

**Detection**: REPL/Eval appear in `system/init.tools[]` when Code Mode is active (MEDIUM — real tools in the pool, `chunk-hp1k7mph.js:205`).

```rust
pub struct SessionOptions {
    pub code_mode: bool,  // sets CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1
    pub tool_search: bool, // sets ENABLE_TOOL_SEARCH=1 (adds tool-search-tool-2025-10-19 beta)
}
```

---

## Workflow / Background Tasks (HIGH)

Source: `chunk-20bv8pca.js:16351–16369` (`M6` schema, `background_tasks_changed`), `chunk-20bv8pca.js:16307–16311` (`task_type`, `workflow_name` fields), `chunk-20bv8pca.js:20970–20978` (`fl` schema, `background_tasks` control request)

**`background_tasks_changed`** is a **level signal** (replace semantics — entire list replaces prior). Fields include `parent_task_id` for the full agent tree.

**`background_tasks` control request** (`chunk-20bv8pca.js:20970–20978`): with `tool_use_id` → backgrounds one specific task; without it → backgrounds all foreground tasks (Ctrl+B semantics). The description says: *"Each blocking tool call returns immediately with a 'running in the background' tool_result and the turn continues."*

**Workflow task type**: `task_type: "local_workflow"` in `background_tasks_changed` events (`chunk-20bv8pca.js:16307–16311`). `workflow_name` carries `meta.name` from the workflow script.

```rust
pub struct BackgroundTask {
    pub id: String,
    pub parent_task_id: Option<String>,
    pub task_type: TaskType,  // "local_workflow" | "agent" | ...
    pub status: TaskStatus,
    pub description: Option<String>,
}
```

---

## Tool Search (HIGH)

Source: search for `ENABLE_TOOL_SEARCH` in source; beta `tool-search-tool-2025-10-19` appears in `system/init.betas[]`.

Set `ENABLE_TOOL_SEARCH=1` in subprocess env to activate. Distinct from Code Mode.

---

## `sdkMcpServerManifests` (HIGH)

Source: `chunk-20bv8pca.js:17732–17752` (schema + docstring), `chunk-20bv8pca.js:18054` (response field `sdk_mcp_manifests_parked`)

Pre-load MCP server handshake results in the `initialize` message to skip N×2 round-trips at startup. The docstring at 17747 says: *"MCP handshake results the host already obtained from its in-process servers by delivering initialize + notifications/initialized (+ tools/list) to them itself before writing this request. For each such server the CLI answers its own MCP client's initialize and first tools/list from these results and skips the notifications/initialized round trip."*

Response field `sdk_mcp_manifests_parked` reports per-entry outcome: `"parked"` | `"already_connected"` | `"protocol_version_mismatch"` | `"malformed"` | `"not_honoured"` (`chunk-20bv8pca.js:18054`).

---

## Semantic Compaction via `PreCompact` Hook (HIGH)

Source: `chunk-20bv8pca.js` hook list; `compact_boundary` schema line 15408

When context nears the limit, the CLI fires a `PreCompact` hook before compacting. The host can substitute a custom summary (from a separate summarizer session) instead of the default compaction. This enables semantic compaction.

---

## Architecture

```
claude-sdk-rs/
├── src/
│   ├── lib.rs
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
├── claude-sdk-macros/   # #[claude_tool] → <fn>_tool_def() + <fn>_tool()
└── tests/               # integration_test.rs (live CLI, --ignored), replay_test.rs, macro_usage.rs
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

## Lifecycle

The Session owns the only strong stdin sender and the reader task owns the only strong
event sender. `close()` (or dropping the last Session clone) closes stdin, so the CLI
exits; `close()` waits for the exit and returns the code. When the CLI's stdout ends,
the event channel closes and in-flight streams end with `Error`.

---

## Closed Items (previously open)

- **`Czn` alias** (`chunk-hp1k7mph.js:69–70`): `gt = "Agent"`, `sf = "Task"` from `chunk-6r0zxzh1.js:3,15`. The init frame renames the tool named `"Agent"` to `"Task"` in the displayed list — no impact on Code Mode detection.
- **`sdkMcpServerManifests` wire format**: fully documented in `chunk-20bv8pca.js:17732–17752` — see section above.
- **Workflow `task_type`**: `"local_workflow"` confirmed in `chunk-20bv8pca.js:16307–16311`.

## Open Items

1. **REPL/Eval in `tools[]`** (MEDIUM): confirm by running a live Code Mode session and capturing the `system/init` frame. The MEDIUM confidence is sufficient for SDK design; detection via env var is HIGH.

