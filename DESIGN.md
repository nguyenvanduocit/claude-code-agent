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

Source: `chunk-20bv8pca.js:17716–17891` (`qfo` schema)

Key fields (30+):

```rust
pub struct Initialize {
    pub api_key_source: ApiKeySource,
    pub betas: Vec<String>,
    pub capabilities: Vec<String>,
    pub cwd: String,
    pub hooks: Vec<HookConfig>,
    pub mcp_server_manifests: Vec<McpManifest>,  // pre-load MCP to skip N×2 round-trips
    pub model: String,
    pub output_style: OutputStyle,
    pub permission_mode: PermissionMode,
    pub per_task_stop_affordance: bool,           // interrupt spares background agents
    pub forward_nested_subagent_tool_calls: bool, // observe full agent tree
    pub session_id: Option<String>,
    pub skills: Vec<SkillConfig>,
    pub tools: Vec<ToolDef>,                      // empty in SDK mode
    pub plugins: Vec<PluginDef>,
    // ... 15+ more
}
```

`per_task_stop_affordance` and `forward_nested_subagent_tool_calls` are unexposed by existing SDKs.

---

## 33 Hook Types (HIGH)

Source: `chunk-20bv8pca.js:12053–12087` (`N2` array)

```
PreToolUse  PostToolUse  PostToolUseFailure  PostToolBatch
PreToolUseBash  PostCompact  PreCompact
UserPromptSubmit  UserPromptExpansion
Stop  StopFailure  SubagentStop  SubagentStart
Notification
Startup  StartupMetrics  StartupMcpServers
FileChanged  CwdChanged  DirectoryChanged
ConversationStored  ScratchpadUpdated
AgentStarted  AgentStopped
WorkspaceCreated  WorkspaceDestroyed
ToolSearchAdded  ToolSearchRemoved
Session  PeerMessage  PeerMessageHold
```

Existing SDKs expose 4–6 of these. This SDK exposes all 33.

### Hook Timeouts (HIGH)

Source: `chunk-0f728242.js:432–448`

| Hook(s) | Timeout |
|---|---|
| `PreToolUse` / `PostToolUse` / `PostToolUseFailure` / `PostToolBatch` | 15 s |
| `UserPromptSubmit` / `UserPromptExpansion` | 30 s |
| `Stop` / `StopFailure` / `SubagentStop` | 120 s |
| All others | default 5 s |

### Hook Callback IPC (HIGH)

Source: `chunk-20bv8pca.js:19959–19979`

CLI calls back with a `hook_callback` control_request containing:
- `event_name`, `tool_name`, `deadline_ms`
- Hook handler returns `{ decision: "approve"|"deny"|"modify", reason?, modified_input? }`

```rust
pub trait HookHandler: Send + Sync {
    async fn handle(
        &self,
        event: HookEvent,
        deadline: Duration,
    ) -> HookDecision;
}
```

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
│   ├── codec.rs         # JSON codec for all 33+ message types
│   ├── session.rs       # Persistent subprocess + readline loop
│   ├── pool.rs          # Session pool, respawn on crash
│   ├── hooks.rs         # HookHandler trait + 33 event types
│   ├── middleware.rs    # tower-style hook middleware stack
│   ├── tools.rs         # Tool definition + #[claude_tool] proc macro
│   ├── background.rs    # background_tasks_changed stream
│   ├── compaction.rs    # PreCompact hook + custom summarizer
│   ├── tracing.rs       # OpenTelemetry spans per turn/hook
│   └── replay.rs        # Record/replay for deterministic tests
└── Cargo.toml
```

---

## Implementation Phases

| Phase | Scope | Key deliverable |
|---|---|---|
| P1 | Codec + persistent session + streaming | `Session::query()` returns `impl Stream<Item=Message>` |
| P2 | 33 hooks + `hook_callback` roundtrip | `HookHandler` trait, all 33 variants, deadlines enforced |
| P3 | Session pool + auto-respawn | `SessionPool` with health checks |
| P4 | Hook middleware stack | tower-compatible layer stack |
| P5 | `#[claude_tool]` proc macro + FS events | `FileChanged`/`CwdChanged` reactive hooks |
| P6 | Semantic compaction | `PreCompact` with summarizer session injection |
| P7 | Subagent tree + `background_tasks` | Full agent tree observable via stream |
| P8 | Record/replay | Deterministic test harness |
| P9 | OpenTelemetry | Per-turn + per-hook spans |

---

## Cargo.toml (skeleton)

```toml
[package]
name = "claude-sdk"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["codec"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
futures = "0.3"
thiserror = "1"
tracing = "0.1"
opentelemetry = { version = "0.22", optional = true }

[dev-dependencies]
tokio-test = "0.4"
```

---

## Closed Items (previously open)

- **`Czn` alias** (`chunk-hp1k7mph.js:69–70`): `gt = "Agent"`, `sf = "Task"` from `chunk-6r0zxzh1.js:3,15`. The init frame renames the tool named `"Agent"` to `"Task"` in the displayed list — no impact on Code Mode detection.
- **`sdkMcpServerManifests` wire format**: fully documented in `chunk-20bv8pca.js:17732–17752` — see section above.
- **Workflow `task_type`**: `"local_workflow"` confirmed in `chunk-20bv8pca.js:16307–16311`.

## Open Items

1. **REPL/Eval in `tools[]`** (MEDIUM): confirm by running a live Code Mode session and capturing the `system/init` frame. The MEDIUM confidence is sufficient for SDK design; detection via env var is HIGH.

