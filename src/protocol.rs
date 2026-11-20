/// Wire types for the Claude Code stream-json protocol.
/// Source: decompiled Claude Code 2.1.328 (chunk-20bv8pca.js, chunk-f89nsf5q.query.js).
/// Every type carries a file:line citation.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Outbound: host → CLI ────────────────────────────────────────────────────

/// Session init. Sent once after `--await-initialize` prompt.
/// Source: chunk-20bv8pca.js:17716–17891 (qfo schema)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Initialize {
    #[serde(rename = "type")]
    pub kind: String, // always "initialize"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub betas: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    pub cwd: String,
    /// SDK-side function hooks — CLI calls back via hook_callback IPC.
    /// Source: chunk-f89nsf5q.query.js:1953–1963 (hookCallbackIds)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hook_callback_ids: Vec<HookCallbackId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// Interrupt spares background agents instead of killing them all.
    /// Source: chunk-20bv8pca.js (perTaskStopAffordance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_task_stop_affordance: Option<bool>,
    /// Observe the full nested agent tree via SubagentStart/Stop hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_nested_subagent_tool_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDef>,
    /// Pre-loaded MCP manifests — skips N×2 round-trips at spawn time.
    /// Source: chunk-20bv8pca.js:17732–17752
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sdk_mcp_server_manifests: Vec<McpManifest>,
}

impl Initialize {
    pub fn new(cwd: impl Into<String>) -> Self {
        Self {
            kind: "initialize".into(),
            cwd: cwd.into(),
            ..Default::default()
        }
    }
}

/// A registered SDK-side hook callback.
/// Source: chunk-20bv8pca.js:19959–19979; chunk-mpj2xm2s.js:6
/// ID format: `<instanceId>/<event>/<index>`, regex ^([A-Za-z0-9][A-Za-z0-9._-]{0,63})\/([A-Za-z]{1,64})\/\d{1,4}$
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCallbackId {
    pub id: String,
    pub event: HookEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u32>,
}

/// A user turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub kind: String, // "user"
    pub message: UserMessageBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
}

impl UserMessage {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            kind: "user".into(),
            message: UserMessageBody {
                role: "user".into(),
                content: vec![ContentBlock::Text { text: content.into() }],
            },
            uuid: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageBody {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
    },
}

/// Control message: host → CLI.
/// Source: chunk-20bv8pca.js:12030–12046 (can_use_tool response)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    #[serde(rename = "type")]
    pub kind: String, // "control_response"
    pub request_id: String,
    pub subtype: String,
    #[serde(flatten)]
    pub payload: ControlResponsePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ControlResponsePayload {
    CanUseTool(ToolDecision),
    HookCallback(HookCallbackReply),
}

/// Allow or deny a tool call.
/// Source: chunk-20bv8pca.js:12030–12046
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior")]
pub enum ToolDecision {
    #[serde(rename = "allow")]
    Allow {
        #[serde(skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    #[serde(rename = "deny")]
    Deny {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
}

/// Reply to a hook_callback IPC call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCallbackReply {
    pub callback_id: String,
    pub output: HookOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookOutput {
    /// exit-code-2 equivalent: stderr injected to model, loop continues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_message: Option<String>,
    /// Additional context injected as a system message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// watchPaths for FileChanged/CwdChanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch_paths: Vec<String>,
    /// For PreCompact: custom summarization instructions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_instructions: Option<String>,
}

impl HookOutput {
    pub fn allow() -> Self {
        Self { block_message: None, additional_context: None, watch_paths: vec![], compact_instructions: None }
    }
    pub fn block(msg: impl Into<String>) -> Self {
        Self { block_message: Some(msg.into()), ..Self::allow() }
    }
    pub fn with_context(ctx: impl Into<String>) -> Self {
        Self { additional_context: Some(ctx.into()), ..Self::allow() }
    }
    pub fn compact_instructions(instr: impl Into<String>) -> Self {
        Self { compact_instructions: Some(instr.into()), ..Self::allow() }
    }
}

/// Background a specific task (Ctrl+B equivalent).
/// Source: chunk-20bv8pca.js:20970–20978
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTasksRequest {
    #[serde(rename = "type")]
    pub kind: String, // "control_request"
    pub subtype: String, // "background_tasks"
    /// None = background all foreground tasks; Some = background specific tool_use_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl BackgroundTasksRequest {
    pub fn all() -> Self {
        Self { kind: "control_request".into(), subtype: "background_tasks".into(), tool_use_id: None }
    }
    pub fn one(tool_use_id: impl Into<String>) -> Self {
        Self { kind: "control_request".into(), subtype: "background_tasks".into(), tool_use_id: Some(tool_use_id.into()) }
    }
}

/// Interrupt a running turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptRequest {
    #[serde(rename = "type")]
    pub kind: String,  // "control_request"
    pub subtype: String, // "interrupt"
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_queued: Option<bool>,
}

// ─── Inbound: CLI → host ─────────────────────────────────────────────────────

/// Every stdout line deserializes to this.
/// Source: chunk-20bv8pca.js (various schema locations)
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum InboundMessage {
    /// Per-turn metadata. First frame before each turn.
    #[serde(rename = "system")]
    System(SystemMessage),
    /// Complete assistant message block.
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    /// Partial streaming chunk (requires --include-partial-messages).
    #[serde(rename = "stream_event")]
    StreamEvent(StreamEventMessage),
    /// Turn complete signal.
    #[serde(rename = "result")]
    Result(ResultMessage),
    /// CLI asking host to approve/deny a tool call.
    /// Source: chunk-20bv8pca.js:18214
    #[serde(rename = "control_request")]
    ControlRequest(ControlRequest),
    /// Context compaction checkpoint.
    #[serde(rename = "compact_boundary")]
    CompactBoundary(CompactBoundaryMessage),
    /// Extended thinking partial (needs thinking betas).
    #[serde(rename = "thinking_tokens")]
    ThinkingTokens { content: String },
    /// Background task tree update (level signal — replace semantics).
    /// Source: chunk-20bv8pca.js:16351–16369
    #[serde(rename = "background_tasks_changed")]
    BackgroundTasksChanged(BackgroundTasksChangedMessage),
    /// Model was switched during the turn.
    #[serde(rename = "model_fallback")]
    ModelFallback { from_model: Option<String>, to_model: String },
    /// Model refused with no fallback.
    #[serde(rename = "model_refusal_no_fallback")]
    ModelRefusalNoFallback { reason: Option<String> },
    /// Process keepalive with startup marker.
    /// Source: chunk-f89nsf5q.query.js:1403–1410
    #[serde(rename = "keep_alive")]
    KeepAlive(KeepAliveMessage),
    /// Multi-session coordination primitive.
    /// Source: chunk-20bv8pca.js:15972
    #[serde(rename = "peer_message_hold")]
    PeerMessageHold(serde_json::Value),
    #[serde(other)]
    Unknown,
}

/// Per-turn metadata frame.
/// Source: chunk-hp1k7mph.js:194 (WGt function), chunk-20bv8pca.js:15125
#[derive(Debug, Clone, Deserialize)]
pub struct SystemMessage {
    pub subtype: String, // "init"
    pub cwd: String,
    pub session_id: String,
    pub model: String,
    pub claude_code_version: String,
    pub permission_mode: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub betas: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub skills: Vec<serde_json::Value>,
    #[serde(default)]
    pub plugins: Vec<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl SystemMessage {
    /// MEDIUM confidence: REPL/Eval in tools[] = Code Mode active.
    /// HIGH confidence: check env var CLAUDE_CODE_EXPERIMENTAL_CODE_MODE.
    pub fn code_mode_active(&self) -> bool {
        self.tools.iter().any(|t| t == "REPL" || t == "Eval")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessage {
    pub message: AssistantMessageBody,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistantMessageBody {
    pub role: String,
    pub content: Vec<AssistantBlock>,
    #[serde(default)]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(other)]
    Unknown,
}

/// Partial streaming chunk.
/// Source: chunk-20bv8pca.js:15343–15401
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEventMessage {
    pub event: serde_json::Value,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

/// Turn complete.
/// Source: chunk-20bv8pca.js:14425–14913
#[derive(Debug, Clone, Deserialize)]
pub struct ResultMessage {
    pub subtype: String, // "success" | "error_*"
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub usage: Option<UsageSummary>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl ResultMessage {
    pub fn is_success(&self) -> bool { self.subtype == "success" }
}

/// Token usage breakdown.
/// Source: chunk-20bv8pca.js:11526–11530
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation: Option<serde_json::Value>,
}

/// CLI→host control request (tool permission or hook callback).
/// Source: chunk-20bv8pca.js:18214 (can_use_tool), :19959–19979 (hook_callback)
#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequest {
    pub subtype: String,       // "can_use_tool" | "hook_callback"
    pub request_id: String,
    #[serde(flatten)]
    pub payload: ControlRequestPayload,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ControlRequestPayload {
    CanUseTool(CanUseToolRequest),
    HookCallback(HookCallbackRequest),
    Other(serde_json::Value),
}

#[derive(Debug, Clone, Deserialize)]
pub struct CanUseToolRequest {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

/// Hook callback IPC from CLI.
/// Source: chunk-20bv8pca.js:19959–19979
#[derive(Debug, Clone, Deserialize)]
pub struct HookCallbackRequest {
    pub callback_id: String,          // "<instanceId>/<event>/<index>"
    pub input: HookEventPayload,
    pub issued_at: u64,
    pub deadline_ms: u64,
}

/// Input to a hook handler. Contains all event-specific fields.
/// Source: chunk-0msz7hbn.js:7–323
#[derive(Debug, Clone, Deserialize)]
pub struct HookEventPayload {
    pub hook_event_name: HookEventKind,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub is_interrupt: Option<bool>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub file_path: Option<String>,
    #[serde(default)]
    pub event: Option<String>,        // for FileChanged: "change"|"add"|"unlink"
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Context-compaction checkpoint.
/// Source: chunk-20bv8pca.js:15408
#[derive(Debug, Clone, Deserialize)]
pub struct CompactBoundaryMessage {
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Background task tree snapshot (replace semantics).
/// Source: chunk-20bv8pca.js:16307–16311, 16351–16369
#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundTasksChangedMessage {
    pub tasks: Vec<BackgroundTask>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundTask {
    pub id: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,   // "local_workflow" | "agent" | ...
    #[serde(default)]
    pub workflow_name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Process keepalive with startup marker for readiness probing.
/// Source: chunk-f89nsf5q.query.js:1403–1410
#[derive(Debug, Clone, Deserialize)]
pub struct KeepAliveMessage {
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ─── Tool/Hook types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpManifest {
    pub name: String,
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub server_info: Option<serde_json::Value>,
}

/// All 33 hook event kinds.
/// Source: chunk-20bv8pca.js:12053–12087 (N2 array)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HookEventKind {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    Notification,
    UserPromptSubmit,
    UserPromptExpansion,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    PreModelSwitch,
    PostModelSwitch,
    PermissionRequest,
    PermissionDenied,
    Setup,
    TeammateIdle,
    TaskCreated,
    TaskCompleted,
    Elicitation,
    ElicitationResult,
    ConfigChange,
    WorktreeCreate,
    WorktreeRemove,
    InstructionsLoaded,
    CwdChanged,
    FileChanged,
    DirectoryAdded,
    MessageDisplay,
}

impl HookEventKind {
    /// Default timeout for this event kind.
    /// Source: chunk-0f728242.js:432–448
    pub fn default_timeout_ms(&self) -> u64 {
        match self {
            HookEventKind::PreToolUse
            | HookEventKind::PostToolUse
            | HookEventKind::PostToolUseFailure
            | HookEventKind::PostToolBatch
            | HookEventKind::PermissionDenied
            | HookEventKind::SubagentStart => 15_000,
            HookEventKind::UserPromptSubmit | HookEventKind::UserPromptExpansion => 30_000,
            HookEventKind::Stop | HookEventKind::StopFailure | HookEventKind::SubagentStop => {
                120_000
            }
            _ => 10_000, // conservative default for unspecified events
        }
    }
}
