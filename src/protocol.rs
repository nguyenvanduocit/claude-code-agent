//! Wire types for the Claude Code stream-json protocol.
//! Source: the Claude Code 2.1.328 binary's zod schemas and the official
//! Python SDK (claude_agent_sdk 0.1.48); citations are on the types they cover.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Outbound: host → CLI ────────────────────────────────────────────────────
//
// Wire format verified against the official Python SDK
// (claude_agent_sdk 0.1.48, _internal/query.py:150–160, :238–375) and the
// zod schemas inside the Claude Code 2.1.328 binary.

/// Envelope for every host → CLI control request:
/// `{"type":"control_request","request_id":"…","request":{"subtype":"…",…}}`.
/// Source: query.py:368–372
#[derive(Debug, Clone, Serialize)]
pub struct OutboundControlRequest<'a, B: Serialize> {
    #[serde(rename = "type")]
    pub kind: &'static str, // "control_request"
    pub request_id: &'a str,
    pub request: &'a B,
}

/// Body of the `initialize` control request — the first stdin line.
///
/// Process-level settings (cwd, model, permission mode, session id) are CLI
/// flags, not initialize fields; they live on `SessionOptions`.
/// `hooks` is filled in by `Session::spawn` from `SessionOptions::hooks`.
/// Source: binary initialize schema (subtype "initialize", hooks, agents,
/// systemPrompt, appendSystemPrompt, jsonSchema, …); query.py:150–155
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Initialize {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HashMap<HookEventKind, Vec<HookMatcherConfig>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    /// Subagent definitions, keyed by agent name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<serde_json::Value>,
    /// JSON schema for structured output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
}

/// One hook matcher in the initialize request.
/// Source: query.py:142–147 (`matcher`, `hookCallbackIds`, `timeout`)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookMatcherConfig {
    pub matcher: Option<String>,
    pub hook_callback_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Permission modes accepted by `--permission-mode` and `set_permission_mode`.
/// Source: binary permission-mode enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
    DontAsk,
    Auto,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::Plan => "plan",
            Self::DontAsk => "dontAsk",
            Self::Auto => "auto",
        }
    }
}

/// A user turn.
/// Source: _internal/client.py:126–131
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessage {
    #[serde(rename = "type")]
    pub kind: String, // "user"
    pub message: UserMessageBody,
    pub parent_tool_use_id: Option<String>,
    /// Echoed back (`isReplay: true`) when the CLI starts this message's turn,
    /// because the session runs with `--replay-user-messages`.
    pub uuid: String,
}

impl UserMessage {
    pub fn text(content: impl Into<String>) -> Self {
        Self::blocks(vec![ContentBlock::Text { text: content.into() }])
    }

    /// A message with several blocks, e.g. text plus images.
    pub fn blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            kind: "user".into(),
            message: UserMessageBody { role: "user".into(), content },
            parent_tool_use_id: None,
            uuid: uuid::Uuid::new_v4().to_string(),
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
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
    },
}

impl ContentBlock {
    /// An image from base64 data; `media_type` e.g. "image/png".
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Base64 { media_type: media_type.into(), data: data.into() },
        }
    }
}

/// Anthropic Messages API image source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

/// Envelope for every host → CLI answer to a CLI-initiated control request.
/// Source: query.py:325–345
#[derive(Debug, Clone, Serialize)]
pub struct ControlResponse {
    #[serde(rename = "type")]
    pub kind: &'static str, // "control_response"
    pub response: ControlResponseBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "subtype", rename_all = "lowercase")]
pub enum ControlResponseBody {
    Success { request_id: String, response: serde_json::Value },
    Error { request_id: String, error: String },
}

impl ControlResponse {
    pub fn success(request_id: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            kind: "control_response",
            response: ControlResponseBody::Success { request_id: request_id.into(), response },
        }
    }
    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            kind: "control_response",
            response: ControlResponseBody::Error { request_id: request_id.into(), error: error.into() },
        }
    }
}

/// Answer to a `can_use_tool` request.
/// Source: query.py:265–281 (`behavior`, `updatedInput`, `message`, `interrupt`)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "camelCase")]
pub enum ToolDecision {
    /// `updated_input: None` → the original input is sent back unchanged.
    #[serde(rename = "allow")]
    Allow {
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
    },
    #[serde(rename = "deny")]
    Deny {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
    },
}

impl ToolDecision {
    pub fn allow() -> Self { Self::Allow { updated_input: None } }
    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny { message: message.into(), interrupt: None }
    }
}

/// Hook result — the same JSON a shell hook prints on stdout.
/// Source: binary hook-output schema (continue, suppressOutput, stopReason,
/// decision, systemMessage, reason, hookSpecificOutput).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    /// `false` stops the whole agent loop.
    #[serde(rename = "continue", skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<HookDecision>,
    /// Warning shown to the user.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    /// Reason fed back to the model when `decision` is `block`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Event-specific output; must carry `hookEventName`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    Approve,
    Block,
}

impl HookOutput {
    /// No opinion — the CLI proceeds as if the hook printed nothing.
    pub fn allow() -> Self { Self::default() }

    /// Block the action; `reason` is shown to the model.
    pub fn block(reason: impl Into<String>) -> Self {
        Self { decision: Some(HookDecision::Block), reason: Some(reason.into()), ..Self::default() }
    }

    /// Inject `additionalContext` for the given event.
    pub fn with_context(event: HookEventKind, context: impl Into<String>) -> Self {
        Self {
            hook_specific_output: Some(serde_json::json!({
                "hookEventName": event,
                "additionalContext": context.into(),
            })),
            ..Self::default()
        }
    }

    /// Register `watchPaths` for FileChanged hooks (valid for CwdChanged / FileChanged).
    pub fn watch_paths(event: HookEventKind, paths: Vec<String>) -> Self {
        Self {
            hook_specific_output: Some(serde_json::json!({
                "hookEventName": event,
                "watchPaths": paths,
            })),
            ..Self::default()
        }
    }

    pub fn is_block(&self) -> bool { self.decision == Some(HookDecision::Block) }
}

/// Body of a `background_tasks` control request (Ctrl+B equivalent).
/// Send with `Session::control(&body)`.
/// Source: chunk-20bv8pca.js:20970–20978
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundTasksRequest {
    pub subtype: &'static str, // "background_tasks"
    /// None = background all foreground tasks; Some = background specific tool_use_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl BackgroundTasksRequest {
    pub fn all() -> Self {
        Self { subtype: "background_tasks", tool_use_id: None }
    }
    pub fn one(tool_use_id: impl Into<String>) -> Self {
        Self { subtype: "background_tasks", tool_use_id: Some(tool_use_id.into()) }
    }
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
    /// A user message: the echo of a host prompt (`is_replay`) or a tool result.
    #[serde(rename = "user")]
    User(InboundUserMessage),
    /// Complete assistant message block.
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    /// Partial streaming chunk (requires --include-partial-messages).
    #[serde(rename = "stream_event")]
    StreamEvent(StreamEventMessage),
    /// Turn complete signal.
    #[serde(rename = "result")]
    Result(ResultMessage),
    /// CLI asking the host to answer: a hook callback, a tool permission
    /// (`can_use_tool`) or an SDK MCP message. Source: chunk-20bv8pca.js:18214
    #[serde(rename = "control_request")]
    ControlRequest(ControlRequest),
    /// CLI answering a host-initiated control request (initialize, interrupt, …).
    /// Source: query.py:180–192
    #[serde(rename = "control_response")]
    ControlResponse(InboundControlResponse),
    /// Process keepalive with startup marker.
    /// Source: chunk-f89nsf5q.query.js:1403–1410
    #[serde(rename = "keep_alive")]
    KeepAlive(KeepAliveMessage),
    #[serde(other)]
    Unknown,
}

/// Every `type:"system"` frame: `init` (first frame of each turn) plus
/// `hook_started`, `hook_response`, `compact_boundary`, `model_fallback`,
/// `background_tasks_changed`, `task_started`, `task_updated`, `task_notification`,
/// `thinking_tokens`, `model_refusal_no_fallback`, `peer_message_hold`, … —
/// only `subtype` and `session_id` are common to all; subtype-specific fields
/// are in `extra`. Source: binary system-message schemas
/// Source: chunk-hp1k7mph.js:194 (WGt function), chunk-20bv8pca.js:15125
#[derive(Debug, Clone, Deserialize)]
pub struct SystemMessage {
    pub subtype: String, // "init" | "hook_started" | "hook_response" | …
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub claude_code_version: String,
    #[serde(default)]
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
    /// `REPL` or `Eval` in `tools[]` = Code Mode active (see `SessionOptions::code_mode`).
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

#[derive(Debug, Clone, Deserialize)]
pub struct InboundUserMessage {
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default, rename = "isReplay")]
    pub is_replay: Option<bool>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
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
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResultMessage {
    /// "success" | "error_*". A failed API call still comes as "success"
    /// with `is_error: true` and the message in `result`.
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub is_error: bool,
    /// Final text of the turn, or the error message when `is_error`.
    #[serde(default)]
    pub result: Option<String>,
    /// Error details of `error_*` subtypes.
    #[serde(default)]
    pub errors: Option<Vec<serde_json::Value>>,
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
    /// Set when the CLI started the turn itself (e.g. `{"kind":"task-notification"}`
    /// after a background task finished); absent for turns the host prompted.
    #[serde(default)]
    pub origin: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl ResultMessage {
    pub fn is_success(&self) -> bool {
        self.subtype == "success" && !self.is_error
    }

    /// Why the turn failed: for `is_error` the CLI's message; for
    /// `error_*` subtypes the subtype, then its `errors` details.
    pub fn error_message(&self) -> String {
        if let Some(r) = self.result.as_ref().filter(|r| self.is_error && !r.is_empty()) {
            return r.clone();
        }
        let details: Vec<String> = self.errors.iter().flatten()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .collect();
        if details.is_empty() { self.subtype.clone() } else { format!("{}: {}", self.subtype, details.join("; ")) }
    }
}

/// Token usage breakdown.
/// Source: chunk-20bv8pca.js:11526–11530
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UsageSummary {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default)]
    pub cache_creation: Option<serde_json::Value>,
}

/// CLI → host control request envelope:
/// `{"type":"control_request","request_id":"…","request":{"subtype":"…",…}}`.
/// Source: query.py:236–240
#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    pub request: ControlRequestBody,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "subtype")]
pub enum ControlRequestBody {
    /// Only sent when the CLI runs with `--permission-prompt-tool stdio`.
    #[serde(rename = "can_use_tool")]
    CanUseTool(CanUseToolRequest),
    #[serde(rename = "hook_callback")]
    HookCallback(HookCallbackRequest),
    /// JSON-RPC message for an SDK MCP server. Source: query.py:305–320
    #[serde(rename = "mcp_message")]
    McpMessage(McpMessageRequest),
    /// Future subtypes — answered with an error response.
    #[serde(other)]
    Unsupported,
}

/// Source: binary can_use_tool schema; query.py:244–262
#[derive(Debug, Clone, Deserialize)]
pub struct CanUseToolRequest {
    pub tool_name: String,
    pub input: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub permission_suggestions: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub decision_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpMessageRequest {
    pub server_name: String,
    pub message: serde_json::Value,
}

/// Source: binary hook_callback schema (callback_id, input, tool_use_id, issued_at)
#[derive(Debug, Clone, Deserialize)]
pub struct HookCallbackRequest {
    pub callback_id: String,
    pub input: HookEventPayload,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Epoch ms on the CLI's clock when the request was issued.
    #[serde(default)]
    pub issued_at: Option<u64>,
}

/// CLI → host answer to a host-initiated control request.
/// Source: query.py:180–192
#[derive(Debug, Clone, Deserialize)]
pub struct InboundControlResponse {
    pub response: InboundControlResponseBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InboundControlResponseBody {
    pub subtype: String, // "success" | "error"
    pub request_id: String,
    #[serde(default)]
    pub response: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
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

/// One entry of a `background_tasks_changed` snapshot.
/// Source: binary background_tasks_changed schema
#[derive(Debug, Clone, Deserialize)]
pub struct BackgroundTask {
    pub task_id: String,
    #[serde(default)]
    pub run_id: Option<String>,
    /// "local_bash" | "local_agent" | "local_workflow" | …
    #[serde(default)]
    pub task_type: String,
    #[serde(default)]
    pub subagent_type: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    #[serde(default)]
    pub ambient: Option<bool>,
    #[serde(default)]
    pub awaited: Option<bool>,
    #[serde(default)]
    pub off_host: Option<bool>,
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

