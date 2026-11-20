//! Wire types for the Claude Code stream-json protocol.
//! Source: the Claude Code 2.1.329 binary's zod schemas and the official
//! Python SDK (claude_agent_sdk 0.1.48); citations are on the types they cover.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Outbound: host → CLI ────────────────────────────────────────────────────
//
// Wire format verified against the official Python SDK
// (claude_agent_sdk 0.1.48, _internal/query.py:150–160, :238–375) and the
// zod schemas inside the Claude Code 2.1.329 binary.

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
    /// JSON schema for structured output; the result arrives in
    /// `TurnResult::structured_output`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    /// Session title; skips automatic title generation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Load only the skills with these names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Plugins to load, e.g. `{"type": "local", "path": "./my-plugin"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugins: Option<Vec<serde_json::Value>>,
    /// MCP servers the CLI runs or connects to, keyed by name: stdio
    /// (`{"command": …, "args": […]}`), `http` or `sse` configs.
    /// In-process servers go in `SessionOptions::mcp_servers`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<serde_json::Map<String, serde_json::Value>>,
    /// Replaces the default plan-mode workflow text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_mode_instructions: Option<String>,
    /// Omit per-user dynamic sections (working directory, memory path) from
    /// the system prompt, so it caches across users.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_dynamic_sections: Option<bool>,
    /// Record the system prompt once per conversation and reuse it verbatim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_snapshot: Option<bool>,
    /// Emit `prompt_suggestion` frames after turns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_suggestions: Option<bool>,
    /// Emit progress summaries of running subagents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_progress_summaries: Option<bool>,
    /// Forward subagent text and thinking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forward_subagent_text: Option<bool>,
    /// Tool-name aliases applied before tool-name resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_aliases: Option<HashMap<String, String>>,
    /// `request_user_dialog` kinds `SessionOptions::on_user_dialog` renders;
    /// the CLI sends only these kinds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_dialog_kinds: Option<Vec<String>>,
    /// Declares that the host renders a per-task stop control (`stop_task`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_task_stop_affordance: Option<bool>,
    /// Workspace trust the host obtained from the user for `directory`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_trust: Option<WorkspaceTrust>,
}

/// Send `accepted: true` only after the user accepted a trust dialog for
/// `directory`. Source: binary initialize.workspaceTrust schema
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceTrust {
    pub accepted: bool,
    pub directory: String,
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

/// Effort levels accepted by `--effort` and `set_effort`.
/// Source: `claude --help` (2.1.329)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// `--thinking`. Source: binary `--thinking <mode>` option (hidden)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thinking {
    Enabled,
    Adaptive,
    Disabled,
}

impl Thinking {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Adaptive => "adaptive",
            Self::Disabled => "disabled",
        }
    }
}

/// How thinking content appears (`--thinking-display`, `set_max_thinking_tokens`).
/// Source: binary thinking-display enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
    Highlights,
}

impl ThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
            Self::Highlights => "highlights",
        }
    }
}

/// `--autocompact`: when the CLI compacts the context on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Autocompact {
    /// The CLI's default window.
    Auto,
    /// Compact at this many tokens (at least 100 000).
    Tokens(u64),
}

impl Autocompact {
    pub fn to_arg(self) -> String {
        match self {
            Self::Auto => "auto".into(),
            Self::Tokens(n) => n.to_string(),
        }
    }
}

/// `--permission-prompts`: who answers a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPrompts {
    /// The SDK host (`SessionOptions::can_use_tool`).
    Host,
    /// Nobody: anything that would prompt is denied.
    None,
}

impl PermissionPrompts {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::None => "none",
        }
    }
}

/// When a prompt sent during a running turn is taken in.
/// Source: binary user-message `priority` enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Aborts the running turn, then runs. The aborted turn still ends with
    /// a `success` result (`terminal_reason: "aborted_streaming"`).
    Now,
    /// Taken in at the next opportunity inside the running turn.
    Next,
    /// Queued until the running turn ends.
    Later,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// `Some(false)`: append to the conversation without starting a turn.
    #[serde(default, rename = "shouldQuery", skip_serializing_if = "Option::is_none")]
    pub should_query: Option<bool>,
    /// ISO 8601 time the message was created. Part of the wire schema, but
    /// CLI 2.1.329 records its own receive time in the echo and transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// `Some(true)`: the host composed this text (the user did not type it).
    /// The CLI delivers it as written: no `@path` expansion, no slash-command
    /// dispatch, and no turn-start context attachments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_composed: Option<bool>,
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
            priority: None,
            should_query: None,
            timestamp: None,
            client_composed: None,
        }
    }

    /// Creation time as an ISO 8601 string, e.g. "2026-11-20T07:03:32.670Z".
    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Mark the text as host-composed: delivered verbatim, with no `@path`
    /// expansion, slash-command dispatch or turn-start attachments.
    pub fn client_composed(mut self) -> Self {
        self.client_composed = Some(true);
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Add the message to the conversation without starting a turn. Its
    /// content is seen by the model on the next prompt.
    pub fn context_only(mut self) -> Self {
        self.should_query = Some(false);
        self
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
    /// A PDF or plain-text document.
    #[serde(rename = "document")]
    Document {
        source: DocumentSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Vec<ContentBlock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

impl ContentBlock {
    /// An image from base64 data; `media_type` e.g. "image/png".
    pub fn image_base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Image {
            source: ImageSource::Base64 { media_type: media_type.into(), data: data.into() },
        }
    }

    /// A PDF from base64 data.
    pub fn pdf_base64(data: impl Into<String>) -> Self {
        Self::Document {
            source: DocumentSource::Base64 { media_type: "application/pdf".into(), data: data.into() },
            title: None,
        }
    }
}

/// Anthropic Messages API document source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DocumentSource {
    Base64 { media_type: String, data: String },
    Text { media_type: String, data: String },
    Url { url: String },
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
/// Source: binary can_use_tool response schema (`behavior`, `updatedInput`,
/// `updatedPermissions`, `message`, `interrupt`)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "camelCase")]
pub enum ToolDecision {
    /// `updated_input: None` → the original input is sent back unchanged.
    /// `updated_permissions`: permission updates to apply, e.g. the
    /// request's `permission_suggestions` for "always allow".
    #[serde(rename = "allow")]
    Allow {
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(rename = "updatedPermissions", default, skip_serializing_if = "Option::is_none")]
        updated_permissions: Option<Vec<serde_json::Value>>,
    },
    #[serde(rename = "deny")]
    Deny {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
    },
}

impl ToolDecision {
    pub fn allow() -> Self { Self::Allow { updated_input: None, updated_permissions: None } }
    /// Allow with a rewritten input.
    pub fn allow_with_input(input: serde_json::Value) -> Self {
        Self::Allow { updated_input: Some(input), updated_permissions: None }
    }
    /// Allow and apply permission updates, so the CLI stops asking (pass the
    /// request's `permission_suggestions` for "always allow").
    pub fn allow_and_remember(updates: Vec<serde_json::Value>) -> Self {
        Self::Allow { updated_input: None, updated_permissions: Some(updates) }
    }
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
    /// The CLI withdrew a control request it sent (e.g. a `can_use_tool`
    /// whose turn was interrupted); its answer is no longer wanted.
    #[serde(rename = "control_cancel_request")]
    ControlCancelRequest(ControlCancelRequest),
    /// Any other frame type (`rate_limit_event`, `tool_progress`,
    /// `prompt_suggestion`, `command_lifecycle`, …), kept verbatim.
    #[serde(skip_deserializing)]
    Other(serde_json::Value),
    /// Decoding placeholder; `Session` turns it into `Other`.
    #[serde(other)]
    Unknown,
}

/// Source: binary control_cancel_request schema
#[derive(Debug, Clone, Deserialize)]
pub struct ControlCancelRequest {
    pub request_id: String,
}

/// Every `type:"system"` frame: `init` (first frame of each turn) plus
/// `hook_started`, `hook_response`, `compact_boundary`, `model_fallback`,
/// `background_tasks_changed`, `task_started`, `task_updated`, `task_notification`,
/// `thinking_tokens`, `model_refusal_no_fallback`, `peer_message_hold`, … —
/// only `subtype` and `session_id` are common to all; subtype-specific fields
/// are in `extra`. Source: binary system-message schemas
/// Source: chunk-hp1k7mph.js:194 (WGt function), chunk-20bv8pca.js:15125
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Encrypted thinking; opaque to the host.
    #[serde(rename = "redacted_thinking")]
    RedactedThinking { data: String },
    /// A tool the API runs itself (web search, …).
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

/// A `user` frame: the echo of a prompt, tool results, or a message the
/// CLI added itself (`is_synthetic`).
#[derive(Debug, Clone, Deserialize)]
pub struct InboundUserMessage {
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default, rename = "isReplay")]
    pub is_replay: Option<bool>,
    #[serde(default, rename = "isSynthetic")]
    pub is_synthetic: Option<bool>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    /// Subagent that produced the frame.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub message: Option<InboundUserBody>,
    /// The tool's full structured output (e.g. Bash `{stdout, stderr, …}`),
    /// beside the text sent to the model.
    #[serde(default)]
    pub tool_use_result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InboundUserBody {
    /// A string or an array of content blocks.
    #[serde(default)]
    pub content: serde_json::Value,
}

/// One `tool_result` block of a `user` frame.
#[derive(Debug, Clone, Deserialize)]
pub struct InboundToolResult {
    pub tool_use_id: String,
    /// A string or an array of content blocks.
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub is_error: bool,
}

impl InboundUserMessage {
    /// The `tool_result` blocks of this frame.
    pub fn tool_results(&self) -> Vec<InboundToolResult> {
        let Some(blocks) = self.message.as_ref().and_then(|m| m.content.as_array()) else {
            return vec![];
        };
        blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
            .collect()
    }
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
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u64>,
    /// Why the loop ended ("completed", …).
    #[serde(default)]
    pub terminal_reason: Option<String>,
    /// Output matching `Initialize::json_schema`.
    #[serde(default)]
    pub structured_output: Option<serde_json::Value>,
    /// Tool calls the permission system denied during the turn.
    #[serde(default)]
    pub permission_denials: Vec<serde_json::Value>,
    /// Per-model token and cost breakdown.
    #[serde(default, rename = "modelUsage")]
    pub model_usage: Option<serde_json::Value>,
    /// The prompts (`UserMessage::uuid`) this turn answered.
    #[serde(default)]
    pub user_message_uuids: Vec<String>,
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

    /// Whether this turn answered the prompt `uuid`. A prompt the CLI
    /// refuses up front (e.g. budget spent) gets a result but no echo.
    pub fn answers(&self, uuid: &str) -> bool {
        self.user_message_uuids.iter().any(|u| u.eq_ignore_ascii_case(uuid))
    }

    /// Category of a failed turn.
    pub fn error_kind(&self) -> TurnErrorKind {
        match self.subtype.as_str() {
            "error_during_execution" => TurnErrorKind::Execution,
            "error_max_turns" => TurnErrorKind::MaxTurns,
            "error_max_budget_usd" => TurnErrorKind::MaxBudget,
            "error_max_structured_output_retries" => TurnErrorKind::StructuredOutputRetries,
            "success" => TurnErrorKind::Api,
            other => TurnErrorKind::Other(other.to_string()),
        }
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

/// Why a turn failed. Source: binary result error-subtype enum
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnErrorKind {
    /// `error_during_execution`: includes interrupted turns.
    Execution,
    /// `error_max_turns`: `SessionOptions::max_turns` reached.
    MaxTurns,
    /// `error_max_budget_usd`: `SessionOptions::max_budget_usd` reached.
    MaxBudget,
    /// `error_max_structured_output_retries`.
    StructuredOutputRetries,
    /// The API call failed (`is_error` on a `success` result).
    Api,
    /// The CLI exited before the turn ended.
    SessionClosed,
    /// The prompt was withdrawn before it ran (`Session::cancel_prompt`,
    /// `Session::interrupt_all`).
    Cancelled,
    /// A subtype this SDK does not know.
    Other(String),
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
    /// An MCP server asks the user for input.
    #[serde(rename = "elicitation")]
    Elicitation(ElicitationRequest),
    /// A tool asks the host to render a blocking dialog; sent only for kinds
    /// listed in `Initialize::supported_dialog_kinds`.
    #[serde(rename = "request_user_dialog")]
    RequestUserDialog(UserDialogRequest),
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
    /// Permission updates that would stop the CLI asking again; return them
    /// with `ToolDecision::allow_and_remember` for "always allow".
    #[serde(default)]
    pub permission_suggestions: Option<Vec<serde_json::Value>>,
    /// Human-readable reason the CLI asks; may carry ANSI escapes.
    #[serde(default)]
    pub decision_reason: Option<String>,
    /// Why the ask escalated: "rule", "hook", "safetyCheck", "classifier", …
    #[serde(default)]
    pub decision_reason_type: Option<String>,
    /// MCP server of the tool: `{"name", "source"}`.
    #[serde(default)]
    pub mcp_server: Option<serde_json::Value>,
    /// Path outside the allowed directories that triggered the ask.
    #[serde(default)]
    pub blocked_path: Option<String>,
    #[serde(default)]
    pub classifier_approvable: Option<bool>,
    /// Do not offer "always allow" for this ask.
    #[serde(default)]
    pub suppress_always_allow_rule: Option<bool>,
    /// The ask must not be approvable by a single stray keystroke.
    #[serde(default)]
    pub default_to_no: Option<bool>,
    #[serde(default)]
    pub matched_ask_rule: Option<serde_json::Value>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Subagent that wants to use the tool.
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub requires_user_interaction: Option<bool>,
    /// Fields not named above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Source: binary elicitation schema
#[derive(Debug, Clone, Deserialize)]
pub struct ElicitationRequest {
    pub mcp_server_name: String,
    pub message: String,
    /// "form" or "url".
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub elicitation_id: Option<String>,
    /// JSON schema of the requested form fields.
    #[serde(default)]
    pub requested_schema: Option<serde_json::Value>,
    /// Top-level keys of `requested_schema.properties`, in the server's order.
    #[serde(default)]
    pub property_order: Option<Vec<String>>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Answer to an `elicitation` request. Source: binary elicitation response schema
#[derive(Debug, Clone, Serialize)]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    Accept,
    Decline,
    Cancel,
}

impl ElicitationResponse {
    pub fn accept(content: serde_json::Map<String, serde_json::Value>) -> Self {
        Self { action: ElicitationAction::Accept, content: Some(content) }
    }
    pub fn decline() -> Self {
        Self { action: ElicitationAction::Decline, content: None }
    }
    pub fn cancel() -> Self {
        Self { action: ElicitationAction::Cancel, content: None }
    }
}

/// Source: binary request_user_dialog schema
#[derive(Debug, Clone, Deserialize)]
pub struct UserDialogRequest {
    pub dialog_kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

/// The user's answer to a `request_user_dialog`.
/// Source: binary request_user_dialog response schema (behavior, result)
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserDialogResponse {
    pub behavior: UserDialogBehavior,
    /// Dialog-specific choice, e.g. `"retry_fallback"` for
    /// `refusal_fallback_prompt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserDialogBehavior {
    Completed,
    /// The user dismissed the dialog.
    Cancelled,
}

impl UserDialogResponse {
    pub fn completed(result: impl Into<serde_json::Value>) -> Self {
        Self { behavior: UserDialogBehavior::Completed, result: Some(result.into()) }
    }
    pub fn cancelled() -> Self {
        Self { behavior: UserDialogBehavior::Cancelled, result: None }
    }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// JSON schema of `ToolResult::structured`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// MCP tool hints. Source: MCP 2025-06-18 `ToolAnnotations`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
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

