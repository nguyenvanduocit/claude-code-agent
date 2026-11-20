//! Wire types for the Claude Code stream-json protocol.
//! Source: the Claude Code 2.1.329 binary's zod schemas and the official
//! Python SDK (`claude_agent_sdk` 0.1.48); citations are on the types they cover.
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
    /// Always `"control_request"`.
    #[serde(rename = "type")]
    pub kind: &'static str, // "control_request"
    /// Id the CLI echoes in its `control_response`.
    pub request_id: &'a str,
    /// Request body; its `subtype` names the request.
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
    /// Hook matchers per event; matches arrive as `hook_callback` requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HashMap<HookEventKind, Vec<HookMatcherConfig>>>,
    /// `systemPrompt`: the custom system prompt slot, same as `--system-prompt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// `appendSystemPrompt`: appended to the system prompt.
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
    /// `true` only after the user accepted the trust dialog.
    pub accepted: bool,
    /// Directory the user trusted.
    pub directory: String,
}

/// One hook matcher in the initialize request.
/// Source: query.py:142–147 (`matcher`, `hookCallbackIds`, `timeout`)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookMatcherConfig {
    /// Tool-name pattern the hook applies to; `None` matches every tool.
    pub matcher: Option<String>,
    /// Callback ids the CLI sends back in `hook_callback` requests.
    pub hook_callback_ids: Vec<String>,
    /// Hook timeout in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u32>,
}

/// Permission modes accepted by `--permission-mode` and `set_permission_mode`.
/// Source: binary permission-mode enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    /// `default`: standard behavior; prompts for dangerous operations.
    Default,
    /// `acceptEdits`: auto-accepts file edits.
    AcceptEdits,
    /// `bypassPermissions`: bypasses all permission checks.
    BypassPermissions,
    /// `plan`: planning mode, no actual tool execution.
    Plan,
    /// `dontAsk`: never prompts; denies anything not pre-approved.
    DontAsk,
    /// `auto`: a model classifier approves or denies permission prompts.
    Auto,
}

impl PermissionMode {
    /// Wire value, e.g. `"acceptEdits"`.
    #[must_use]
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
    /// `low`.
    Low,
    /// `medium`.
    Medium,
    /// `high`.
    High,
    /// `xhigh`.
    Xhigh,
    /// `max`.
    Max,
}

impl Effort {
    /// Wire value, e.g. `"xhigh"`.
    #[must_use]
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
    /// `enabled`.
    Enabled,
    /// `adaptive`.
    Adaptive,
    /// `disabled`.
    Disabled,
}

impl Thinking {
    /// Wire value, e.g. `"adaptive"`.
    #[must_use]
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
    /// `summarized`.
    Summarized,
    /// `omitted`.
    Omitted,
    /// `highlights`.
    Highlights,
}

impl ThinkingDisplay {
    /// Wire value, e.g. `"summarized"`.
    #[must_use]
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
    /// CLI argument: `"auto"` or the token count.
    #[must_use]
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
    /// CLI argument: `"host"` or `"none"`.
    #[must_use]
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
    /// Always `"user"`.
    #[serde(rename = "type")]
    pub kind: String, // "user"
    /// Role and content blocks.
    pub message: UserMessageBody,
    /// Tool call this message belongs to; `None` for a top-level prompt.
    pub parent_tool_use_id: Option<String>,
    /// Echoed back (`isReplay: true`) when the CLI starts this message's turn,
    /// because the session runs with `--replay-user-messages`.
    pub uuid: String,
    /// When the CLI takes the message in during a running turn; `None` = CLI default.
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
    /// A plain-text prompt.
    pub fn text(content: impl Into<String>) -> Self {
        Self::blocks(vec![ContentBlock::Text { text: content.into() }])
    }

    /// A message with several blocks, e.g. text plus images.
    #[must_use]
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
    #[must_use]
    pub fn with_timestamp(mut self, timestamp: impl Into<String>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    /// Mark the text as host-composed: delivered verbatim, with no `@path`
    /// expansion, slash-command dispatch or turn-start attachments.
    #[must_use]
    pub fn client_composed(mut self) -> Self {
        self.client_composed = Some(true);
        self
    }

    /// Set when the prompt is taken in during a running turn.
    #[must_use]
    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Add the message to the conversation without starting a turn. Its
    /// content is seen by the model on the next prompt.
    #[must_use]
    pub fn context_only(mut self) -> Self {
        self.should_query = Some(false);
        self
    }
}

/// Message body: `role` plus content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserMessageBody {
    /// Always `"user"`.
    pub role: String,
    /// Content blocks of the message.
    pub content: Vec<ContentBlock>,
}

/// Content block of a user message (Anthropic Messages API shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// Plain text.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// An image.
    #[serde(rename = "image")]
    Image {
        /// Where the image data comes from.
        source: ImageSource,
    },
    /// A PDF or plain-text document.
    #[serde(rename = "document")]
    Document {
        /// Where the document data comes from.
        source: DocumentSource,
        /// Document title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// The result of a tool call, answering a `tool_use` block.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// Id of the `tool_use` block answered.
        tool_use_id: String,
        /// Result content.
        content: Vec<ContentBlock>,
        /// `Some(true)`: the tool call failed.
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DocumentSource {
    /// Base64-encoded data.
    Base64 {
        /// MIME type, e.g. `"application/pdf"`.
        media_type: String,
        /// Base64 data.
        data: String,
    },
    /// Plain text.
    Text {
        /// MIME type, e.g. `"text/plain"`.
        media_type: String,
        /// The text.
        data: String,
    },
    /// A URL the API fetches.
    Url {
        /// Document URL.
        url: String,
    },
}

/// Anthropic Messages API image source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ImageSource {
    /// Base64-encoded data.
    Base64 {
        /// MIME type, e.g. `"image/png"`.
        media_type: String,
        /// Base64 data.
        data: String,
    },
    /// A URL the API fetches.
    Url {
        /// Image URL.
        url: String,
    },
}

/// Envelope for every host → CLI answer to a CLI-initiated control request.
/// Source: query.py:325–345
#[derive(Debug, Clone, Serialize)]
pub struct ControlResponse {
    /// Always `"control_response"`.
    #[serde(rename = "type")]
    pub kind: &'static str, // "control_response"
    /// Success or error, keyed by `request_id`.
    pub response: ControlResponseBody,
}

/// Body of a `control_response`, tagged by `subtype`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "subtype", rename_all = "lowercase")]
pub enum ControlResponseBody {
    /// The request succeeded.
    Success {
        /// Id of the request answered.
        request_id: String,
        /// Response payload.
        response: serde_json::Value,
    },
    /// The request failed.
    Error {
        /// Id of the request answered.
        request_id: String,
        /// Error message.
        error: String,
    },
}

impl ControlResponse {
    /// Successful answer to the request `request_id`.
    pub fn success(request_id: impl Into<String>, response: serde_json::Value) -> Self {
        Self {
            kind: "control_response",
            response: ControlResponseBody::Success { request_id: request_id.into(), response },
        }
    }
    /// Error answer to the request `request_id`.
    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            kind: "control_response",
            response: ControlResponseBody::Error { request_id: request_id.into(), error: error.into() },
        }
    }
}

/// Answer to a `can_use_tool` request.
/// Source: binary `can_use_tool` response schema (`behavior`, `updatedInput`,
/// `updatedPermissions`, `message`, `interrupt`)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "camelCase")]
pub enum ToolDecision {
    /// `updated_input: None` → the original input is sent back unchanged.
    /// `updated_permissions`: permission updates to apply, e.g. the
    /// request's `permission_suggestions` for "always allow".
    #[serde(rename = "allow")]
    Allow {
        /// `updatedInput`: replacement tool input.
        #[serde(rename = "updatedInput", skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        /// `updatedPermissions`: permission updates to apply.
        #[serde(rename = "updatedPermissions", default, skip_serializing_if = "Option::is_none")]
        updated_permissions: Option<Vec<serde_json::Value>>,
    },
    /// Refuse the tool call.
    #[serde(rename = "deny")]
    Deny {
        /// Reason sent back for the denial.
        message: String,
        /// `Some(true)`: also interrupt the turn.
        #[serde(skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
    },
}

impl ToolDecision {
    /// Allow with the original input.
    #[must_use]
    pub fn allow() -> Self { Self::Allow { updated_input: None, updated_permissions: None } }
    /// Allow with a rewritten input.
    #[must_use]
    pub fn allow_with_input(input: serde_json::Value) -> Self {
        Self::Allow { updated_input: Some(input), updated_permissions: None }
    }
    /// Allow and apply permission updates, so the CLI stops asking (pass the
    /// request's `permission_suggestions` for "always allow").
    #[must_use]
    pub fn allow_and_remember(updates: Vec<serde_json::Value>) -> Self {
        Self::Allow { updated_input: None, updated_permissions: Some(updates) }
    }
    /// Deny with `message`.
    pub fn deny(message: impl Into<String>) -> Self {
        Self::Deny { message: message.into(), interrupt: None }
    }
}

/// Hook result — the same JSON a shell hook prints on stdout.
/// Source: binary hook-output schema (continue, suppressOutput, stopReason,
/// decision, systemMessage, reason, hookSpecificOutput).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    /// `false` stops the whole agent loop.
    #[serde(rename = "continue", skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    /// Hide stdout from the transcript (default: false).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suppress_output: Option<bool>,
    /// Message shown when `continue` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// `approve` or `block`.
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

/// `decision` of a hook result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    /// `approve`.
    Approve,
    /// `block`: `reason` is fed back to the model.
    Block,
}

impl HookOutput {
    /// No opinion — the CLI proceeds as if the hook printed nothing.
    #[must_use]
    pub fn allow() -> Self { Self::default() }

    /// Block the action; `reason` is shown to the model.
    pub fn block(reason: impl Into<String>) -> Self {
        Self { decision: Some(HookDecision::Block), reason: Some(reason.into()), ..Self::default() }
    }

    /// Inject `additionalContext` for the given event. Only events where
    /// `HookEventKind::accepts_additional_context` holds carry it; for any
    /// other the CLI rejects the output, so this logs a warning.
    pub fn with_context(event: HookEventKind, context: impl Into<String>) -> Self {
        if !event.accepts_additional_context() {
            tracing::warn!(?event, "the CLI ignores additionalContext for this hook event");
        }
        Self {
            hook_specific_output: Some(serde_json::json!({
                "hookEventName": event,
                "additionalContext": context.into(),
            })),
            ..Self::default()
        }
    }

    /// Register `watchPaths` for `FileChanged` hooks (valid for `CwdChanged` / `FileChanged`).
    #[must_use]
    pub fn watch_paths(event: HookEventKind, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let paths: Vec<String> = paths.into_iter().map(Into::into).collect();
        Self {
            hook_specific_output: Some(serde_json::json!({
                "hookEventName": event,
                "watchPaths": paths,
            })),
            ..Self::default()
        }
    }

    /// Whether `decision` is `block`.
    #[must_use]
    pub fn is_block(&self) -> bool { self.decision == Some(HookDecision::Block) }
}

/// Body of a `background_tasks` control request (Ctrl+B equivalent).
/// Send with `Session::control(&body)`.
/// Source: chunk-20bv8pca.js:20970–20978
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundTasksRequest {
    /// Always `"background_tasks"`.
    pub subtype: &'static str, // "background_tasks"
    /// None = background all foreground tasks; Some = background specific `tool_use_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl BackgroundTasksRequest {
    /// Background all foreground tasks.
    #[must_use]
    pub fn all() -> Self {
        Self { subtype: "background_tasks", tool_use_id: None }
    }
    /// Background the task of `tool_use_id`.
    pub fn one(tool_use_id: impl Into<String>) -> Self {
        Self { subtype: "background_tasks", tool_use_id: Some(tool_use_id.into()) }
    }
}

// ─── Inbound: CLI → host ─────────────────────────────────────────────────────

/// Every stdout line deserializes to this.
/// Source: chunk-20bv8pca.js (various schema locations)
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
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
    /// Decoding placeholder; `Session` turns it into `Other` and delivers
    /// it only for a stdout line that is not JSON.
    #[doc(hidden)]
    #[serde(other)]
    Unknown,
}

/// Source: binary `control_cancel_request` schema
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ControlCancelRequest {
    /// Id of the withdrawn control request.
    pub request_id: String,
}

/// Every `type:"system"` frame: `init` (first frame of each turn) plus
/// `hook_started`, `hook_response`, `compact_boundary`, `model_fallback`,
/// `background_tasks_changed`, `task_started`, `task_updated`, `task_notification`,
/// `thinking_tokens`, `model_refusal_no_fallback`, `peer_message_hold`, … —
/// only `subtype` and `session_id` are common to all; subtype-specific fields
/// are in `extra`. Source: binary system-message schemas
/// Source: chunk-hp1k7mph.js:194 (`WGt` function), chunk-20bv8pca.js:15125
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SystemMessage {
    /// `init`, `hook_started`, `compact_boundary`, …
    pub subtype: String, // "init" | "hook_started" | "hook_response" | …
    /// Session the frame belongs to.
    pub session_id: String,
    /// Working directory (`init`).
    #[serde(default)]
    pub cwd: String,
    /// Active model (`init`).
    #[serde(default)]
    pub model: String,
    /// CLI version (`init`).
    #[serde(default)]
    pub claude_code_version: String,
    /// Permission mode, as its wire value (`init`).
    #[serde(default)]
    pub permission_mode: String,
    /// Available tool names (`init`).
    #[serde(default)]
    pub tools: Vec<String>,
    /// Enabled API betas (`init`).
    #[serde(default)]
    pub betas: Vec<String>,
    /// Protocol capabilities the CLI supports, for feature detection (`init`).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Loaded skills (`init`), kept verbatim.
    #[serde(default)]
    pub skills: Vec<serde_json::Value>,
    /// Loaded plugins (`init`), kept verbatim.
    #[serde(default)]
    pub plugins: Vec<serde_json::Value>,
    /// Subtype-specific fields.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl SystemMessage {
    /// `REPL` or `Eval` in `tools[]` = Code Mode active (see `SessionOptions::code_mode`).
    #[must_use]
    pub fn code_mode_active(&self) -> bool {
        self.tools.iter().any(|t| t == "REPL" || t == "Eval")
    }
}

/// A complete `assistant` frame.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AssistantMessage {
    /// The model message.
    pub message: AssistantMessageBody,
    /// Session the frame belongs to.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Tool call (e.g. a subagent) that produced the message; `None` for the main thread.
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

/// Body of an `assistant` frame.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct AssistantMessageBody {
    /// Always `"assistant"`.
    pub role: String,
    /// Content blocks of the message.
    pub content: Vec<AssistantBlock>,
    /// Why the model stopped, e.g. `"end_turn"`, `"tool_use"`.
    #[serde(default)]
    pub stop_reason: Option<String>,
}

/// Content block of an assistant message.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum AssistantBlock {
    /// Text.
    #[serde(rename = "text")]
    Text {
        /// The text.
        text: String,
    },
    /// A tool call the CLI runs.
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Tool-use id; the matching `tool_result` carries it.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input.
        input: serde_json::Value,
    },
    /// Extended thinking.
    #[serde(rename = "thinking")]
    Thinking {
        /// The thinking text.
        thinking: String,
    },
    /// Encrypted thinking; opaque to the host.
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        /// Encrypted thinking data.
        data: String,
    },
    /// A tool the API runs itself (web search, …).
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        /// Tool-use id.
        id: String,
        /// Server tool name.
        name: String,
        /// Tool input.
        input: serde_json::Value,
    },
    /// Result of a server-run web search.
    #[serde(rename = "web_search_tool_result")]
    WebSearchToolResult {
        /// Id of the `server_tool_use` block answered.
        tool_use_id: String,
        /// Search results, kept verbatim.
        content: serde_json::Value,
    },
    /// Any other block type.
    #[serde(other)]
    Unknown,
}

/// A `user` frame: the echo of a prompt, tool results, or a message the
/// CLI added itself (`is_synthetic`).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct InboundUserMessage {
    /// Message uuid; for an echo, the host prompt's `UserMessage::uuid`.
    #[serde(default)]
    pub uuid: Option<String>,
    /// `isReplay`: echo of a host prompt.
    #[serde(default, rename = "isReplay")]
    pub is_replay: Option<bool>,
    /// `isSynthetic`: the CLI added the message itself.
    #[serde(default, rename = "isSynthetic")]
    pub is_synthetic: Option<bool>,
    /// Tool call this message belongs to.
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    /// Subagent that produced the frame.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Message body.
    #[serde(default)]
    pub message: Option<InboundUserBody>,
    /// The tool's full structured output (e.g. Bash `{stdout, stderr, …}`),
    /// beside the text sent to the model.
    #[serde(default)]
    pub tool_use_result: Option<serde_json::Value>,
}

/// Body of an inbound `user` frame.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct InboundUserBody {
    /// A string or an array of content blocks.
    #[serde(default)]
    pub content: serde_json::Value,
}

/// One `tool_result` block of a `user` frame.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct InboundToolResult {
    /// Id of the `tool_use` block answered.
    pub tool_use_id: String,
    /// A string or an array of content blocks.
    #[serde(default)]
    pub content: serde_json::Value,
    /// The tool call failed.
    #[serde(default)]
    pub is_error: bool,
}

impl InboundUserMessage {
    /// The `tool_result` blocks of this frame.
    #[must_use]
    pub fn tool_results(&self) -> Vec<InboundToolResult> {
        let Some(blocks) = self.message.as_ref().and_then(|m| m.content.as_array()) else {
            return vec![];
        };
        blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
            .filter_map(|b| InboundToolResult::deserialize(b).ok())
            .collect()
    }
}

/// Partial streaming chunk.
/// Source: chunk-20bv8pca.js:15343–15401
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct StreamEventMessage {
    /// Raw Anthropic streaming event (`message_start`, `content_block_delta`, …).
    pub event: serde_json::Value,
    /// Session the frame belongs to.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Time to first token, in ms.
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    /// Tool call (e.g. a subagent) that produced the event; `None` for the main thread.
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
}

/// Turn complete.
/// Source: chunk-20bv8pca.js:14425–14913
#[derive(Debug, Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct ResultMessage {
    /// "success" | "error_*". A failed API call still comes as "success"
    /// with `is_error: true` and the message in `result`.
    #[serde(default)]
    pub subtype: String,
    /// The turn failed; a failed API call sets this on a `success` result.
    #[serde(default)]
    pub is_error: bool,
    /// Final text of the turn, or the error message when `is_error`.
    #[serde(default)]
    pub result: Option<String>,
    /// Error details of `error_*` subtypes.
    #[serde(default)]
    pub errors: Option<Vec<serde_json::Value>>,
    /// Session the turn ran in.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Estimated cost in USD, cumulative across the session's turns.
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Time to first token, in ms.
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    /// Wall-clock duration, in ms.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Token usage.
    #[serde(default)]
    pub usage: Option<UsageSummary>,
    /// Why the model stopped, e.g. `"end_turn"`.
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Time spent in API calls, in ms.
    #[serde(default)]
    pub duration_api_ms: Option<u64>,
    /// Number of agent-loop turns.
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
    /// Fields not named above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl ResultMessage {
    /// `success` subtype without `is_error`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.subtype == "success" && !self.is_error
    }

    /// Whether this turn answered the prompt `uuid`. A prompt the CLI
    /// refuses up front (e.g. budget spent) gets a result but no echo.
    #[must_use]
    pub fn answers(&self, uuid: &str) -> bool {
        self.user_message_uuids.iter().any(|u| u.eq_ignore_ascii_case(uuid))
    }

    /// Category of a failed turn.
    #[must_use]
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
    #[must_use]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct UsageSummary {
    /// Input tokens.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read_input_tokens: Option<u64>,
    /// Input tokens written to the prompt cache.
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u64>,
    /// Cache writes by TTL, kept verbatim.
    #[serde(default)]
    pub cache_creation: Option<serde_json::Value>,
}

/// CLI → host control request envelope:
/// `{"type":"control_request","request_id":"…","request":{"subtype":"…",…}}`.
/// Source: query.py:236–240
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ControlRequest {
    /// Id to answer with in `ControlResponse`.
    pub request_id: String,
    /// Request body, tagged by `subtype`.
    pub request: ControlRequestBody,
}

/// Body of a CLI → host control request, tagged by `subtype`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "subtype")]
#[non_exhaustive]
pub enum ControlRequestBody {
    /// Only sent when the CLI runs with `--permission-prompt-tool stdio`.
    #[serde(rename = "can_use_tool")]
    CanUseTool(CanUseToolRequest),
    /// A hook matched; run the callback `callback_id`.
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

/// Source: binary `can_use_tool` schema; query.py:244–262
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CanUseToolRequest {
    /// Tool the model wants to use.
    pub tool_name: String,
    /// Tool input.
    pub input: serde_json::Value,
    /// Tool-use id of the call.
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
    /// Set when a safety check is involved: `false` = one needs manual approval.
    #[serde(default)]
    pub classifier_approvable: Option<bool>,
    /// Do not offer "always allow" for this ask.
    #[serde(default)]
    pub suppress_always_allow_rule: Option<bool>,
    /// The ask must not be approvable by a single stray keystroke.
    #[serde(default)]
    pub default_to_no: Option<bool>,
    /// User `permissions.ask` rule that forced this prompt: `{source, tool_name, rule_content}`.
    #[serde(default)]
    pub matched_ask_rule: Option<serde_json::Value>,
    /// Permission-display title.
    #[serde(default)]
    pub title: Option<String>,
    /// Short tool or server label for display.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Permission-display subtitle.
    #[serde(default)]
    pub description: Option<String>,
    /// Subagent that wants to use the tool.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// One-tap Approve/Deny must not be offered.
    #[serde(default)]
    pub requires_user_interaction: Option<bool>,
    /// Fields not named above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Source: binary elicitation schema
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct ElicitationRequest {
    /// MCP server asking for input.
    pub mcp_server_name: String,
    /// Text shown to the user.
    pub message: String,
    /// "form" or "url".
    #[serde(default)]
    pub mode: Option<String>,
    /// URL to open (`url` mode).
    #[serde(default)]
    pub url: Option<String>,
    /// Elicitation id from the MCP server.
    #[serde(default)]
    pub elicitation_id: Option<String>,
    /// JSON schema of the requested form fields.
    #[serde(default)]
    pub requested_schema: Option<serde_json::Value>,
    /// Top-level keys of `requested_schema.properties`, in the server's order.
    #[serde(default)]
    pub property_order: Option<Vec<String>>,
    /// Permission-display title from the MCP server.
    #[serde(default)]
    pub title: Option<String>,
    /// Short tool or server label from the MCP server.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Permission-display subtitle from the MCP server.
    #[serde(default)]
    pub description: Option<String>,
    /// Fields not named above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Answer to an `elicitation` request. Source: binary elicitation response schema
#[derive(Debug, Clone, Serialize)]
pub struct ElicitationResponse {
    /// `accept`, `decline` or `cancel`.
    pub action: ElicitationAction,
    /// Form values; sent with `accept`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Map<String, serde_json::Value>>,
}

/// What the user did with an elicitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ElicitationAction {
    /// The user submitted the form.
    Accept,
    /// The user declined.
    Decline,
    /// The user dismissed the request.
    Cancel,
}

impl ElicitationResponse {
    /// Accept with the form values.
    #[must_use]
    pub fn accept(content: serde_json::Map<String, serde_json::Value>) -> Self {
        Self { action: ElicitationAction::Accept, content: Some(content) }
    }
    /// Decline.
    #[must_use]
    pub fn decline() -> Self {
        Self { action: ElicitationAction::Decline, content: None }
    }
    /// Cancel.
    #[must_use]
    pub fn cancel() -> Self {
        Self { action: ElicitationAction::Cancel, content: None }
    }
}

/// Source: binary `request_user_dialog` schema
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct UserDialogRequest {
    /// Which dialog to render; only kinds in `Initialize::supported_dialog_kinds`.
    pub dialog_kind: String,
    /// Dialog-specific data; shape set by `dialog_kind`.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Tool call that raised the dialog.
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

/// The user's answer to a `request_user_dialog`.
/// Source: binary `request_user_dialog` response schema (behavior, result)
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UserDialogResponse {
    /// `completed` or `cancelled`.
    pub behavior: UserDialogBehavior,
    /// Dialog-specific choice, e.g. `"retry_fallback"` for
    /// `refusal_fallback_prompt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

/// How a `request_user_dialog` ended.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UserDialogBehavior {
    /// The user made a choice (`result`).
    Completed,
    /// The user dismissed the dialog.
    Cancelled,
}

impl UserDialogResponse {
    /// The user chose `result`.
    pub fn completed(result: impl Into<serde_json::Value>) -> Self {
        Self { behavior: UserDialogBehavior::Completed, result: Some(result.into()) }
    }
    /// The user dismissed the dialog.
    #[must_use]
    pub fn cancelled() -> Self {
        Self { behavior: UserDialogBehavior::Cancelled, result: None }
    }
}

/// Body of an `mcp_message` request.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct McpMessageRequest {
    /// SDK MCP server the message is for.
    pub server_name: String,
    /// JSON-RPC message.
    pub message: serde_json::Value,
}

/// Source: binary `hook_callback` schema (`callback_id`, input, `tool_use_id`, `issued_at`)
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct HookCallbackRequest {
    /// Callback id from `HookMatcherConfig::hook_callback_ids`.
    pub callback_id: String,
    /// Hook input.
    pub input: HookEventPayload,
    /// Tool-use id, for tool events.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Epoch ms on the CLI's clock when the request was issued.
    #[serde(default)]
    pub issued_at: Option<u64>,
}

/// CLI → host answer to a host-initiated control request.
/// Source: query.py:180–192
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct InboundControlResponse {
    /// Success or error, keyed by `request_id`.
    pub response: InboundControlResponseBody,
}

/// Body of an inbound `control_response`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct InboundControlResponseBody {
    /// `success` or `error`.
    pub subtype: String, // "success" | "error"
    /// Id of the host request answered.
    pub request_id: String,
    /// Response payload on `success`.
    #[serde(default)]
    pub response: Option<serde_json::Value>,
    /// Error message on `error`.
    #[serde(default)]
    pub error: Option<String>,
}

/// Input to a hook handler. Contains all event-specific fields.
/// Source: chunk-0msz7hbn.js:7–323
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct HookEventPayload {
    /// Event that fired.
    pub hook_event_name: HookEventKind,
    /// Tool name, for tool events.
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Tool input, for tool events.
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    /// Tool-use id, for tool events.
    #[serde(default)]
    pub tool_use_id: Option<String>,
    /// Error message, for `PostToolUseFailure`.
    #[serde(default)]
    pub error: Option<String>,
    /// Whether the failure came from an interrupt, for `PostToolUseFailure`.
    #[serde(default)]
    pub is_interrupt: Option<bool>,
    /// Session the event belongs to.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Changed file, for `FileChanged`.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Change kind, for `FileChanged`.
    #[serde(default)]
    pub event: Option<String>,        // for FileChanged: "change"|"add"|"unlink"
    /// Fields not named above.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// One entry of a `background_tasks_changed` snapshot.
/// Source: binary `background_tasks_changed` schema
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct BackgroundTask {
    /// Task id.
    pub task_id: String,
    /// Id of one run of the task; a resumed task gets a new one.
    #[serde(default)]
    pub run_id: Option<String>,
    /// "`local_bash`" | "`local_agent`" | "`local_workflow`" | …
    #[serde(default)]
    pub task_type: String,
    /// Agent type of a `local_agent` task.
    #[serde(default)]
    pub subagent_type: Option<String>,
    /// Task description.
    #[serde(default)]
    pub description: String,
    /// Task id of the subagent that launched this task.
    #[serde(default)]
    pub parent_task_id: Option<String>,
    /// Not activity; exclude from activity indicators.
    #[serde(default)]
    pub ambient: Option<bool>,
    /// A resumed subagent run that the resuming tool call waits for.
    #[serde(default)]
    pub awaited: Option<bool>,
    /// The task's work goes on outside this process.
    #[serde(default)]
    pub off_host: Option<bool>,
}

/// Process keepalive with startup marker for readiness probing.
/// Source: chunk-f89nsf5q.query.js:1403–1410
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct KeepAliveMessage {
    /// Frame fields, kept verbatim.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ─── Tool/Hook types ─────────────────────────────────────────────────────────

/// A tool of an in-process SDK MCP server.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ToolDef {
    /// Tool name.
    pub name: String,
    /// Description shown to the model.
    pub description: String,
    /// JSON schema of the tool input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// Display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// JSON schema of `ToolResult::structured`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// MCP tool hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// MCP tool hints. Source: MCP 2025-06-18 `ToolAnnotations`
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    /// `readOnlyHint`: the tool does not modify its environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    /// `destructiveHint`: the tool may perform destructive updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    /// `idempotentHint`: repeated calls with the same arguments have no extra effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    /// `openWorldHint`: the tool interacts with external entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

/// An MCP server's name and tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpManifest {
    /// Server name.
    pub name: String,
    /// Server tools.
    pub tools: Vec<ToolDef>,
    /// Wire field `server_info`, kept verbatim.
    #[serde(default)]
    pub server_info: Option<serde_json::Value>,
}

/// All 33 hook event kinds.
/// Source: chunk-20bv8pca.js:12053–12087 (N2 array)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
#[non_exhaustive]
pub enum HookEventKind {
    /// Before tool execution.
    PreToolUse,
    /// After tool execution.
    PostToolUse,
    /// After tool execution fails.
    PostToolUseFailure,
    /// After a batch of tool calls resolves.
    PostToolBatch,
    /// When notifications are sent.
    Notification,
    /// When the user submits a prompt.
    UserPromptSubmit,
    /// When a user-typed slash command expands into a prompt.
    UserPromptExpansion,
    /// When a new session is started.
    SessionStart,
    /// When a session is ending.
    SessionEnd,
    /// Right before Claude concludes its response.
    Stop,
    /// When the turn ends due to an API error.
    StopFailure,
    /// When a subagent (Agent tool call) is started.
    SubagentStart,
    /// Right before a subagent (Agent tool call) concludes its response.
    SubagentStop,
    /// Before conversation compaction.
    PreCompact,
    /// After conversation compaction.
    PostCompact,
    /// Before a requested model switch (`/model`, model picker, `set_model`).
    PreModelSwitch,
    /// After the session model changes (any cause).
    PostModelSwitch,
    /// When a permission dialog is displayed.
    PermissionRequest,
    /// After auto mode denies a tool call.
    PermissionDenied,
    /// Repo setup hooks for init and maintenance.
    Setup,
    /// When a teammate is about to go idle.
    TeammateIdle,
    /// When a task is being created.
    TaskCreated,
    /// When a task is being marked as completed.
    TaskCompleted,
    /// When an MCP server requests user input (elicitation).
    Elicitation,
    /// After a user responds to an MCP elicitation.
    ElicitationResult,
    /// When configuration files change during a session.
    ConfigChange,
    /// Create an isolated worktree for VCS-agnostic isolation.
    WorktreeCreate,
    /// Remove a previously created worktree.
    WorktreeRemove,
    /// When an instruction file (CLAUDE.md or rule) is loaded.
    InstructionsLoaded,
    /// After the working directory changes.
    CwdChanged,
    /// When a watched file changes.
    FileChanged,
    /// After a working directory is added mid-session.
    DirectoryAdded,
    /// While assistant message text is displayed.
    MessageDisplay,
}

impl HookEventKind {
    /// Whether the CLI reads `hookSpecificOutput.additionalContext` for
    /// this event (`HookOutput::with_context`).
    /// Source: CLI hook output schema (chunk-g4jweshk.js:10682-10753)
    #[must_use]
    pub fn accepts_additional_context(self) -> bool {
        matches!(
            self,
            Self::PreToolUse
                | Self::PostToolUse
                | Self::PostToolUseFailure
                | Self::PostToolBatch
                | Self::UserPromptSubmit
                | Self::UserPromptExpansion
                | Self::SessionStart
                | Self::Setup
                | Self::PostModelSwitch
                | Self::SubagentStart
                | Self::Stop
                | Self::SubagentStop
                | Self::Notification
        )
    }
}

