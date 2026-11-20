//! In-process (SDK) MCP servers — Rust functions exposed to Claude as tools.
//!
//! The CLI is told about each server with
//! `--mcp-config {"mcpServers":{"<name>":{"type":"sdk","name":"<name>"}}}`
//! (or later with `Session::set_mcp_servers`) and then forwards every
//! JSON-RPC message for it as an `mcp_message` control request; the session
//! answers with `{"mcp_response": <JSON-RPC response>}`.
//! Claude sees the tools as `mcp__<server>__<tool>`.
//!
//! Source: `claude_agent_sdk` 0.1.48 `transport/subprocess_cli.py:240–260`,
//!         _internal/query.py:305–320, :394–500; binary sdk MCP config schema
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::protocol::ToolDef;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type ToolFn = Arc<dyn Fn(Value, ToolContext) -> BoxFuture<ToolResult> + Send + Sync>;

const PROTOCOL_VERSION: &str = "2024-11-05";
/// JSON-RPC error code for a request the client cancelled.
const REQUEST_CANCELLED: i64 = -32800;

/// One content item of a tool result. Source: MCP `CallToolResult.content`
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// Base64 image data, e.g. `mime_type` "image/png".
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// Image MIME type (`mimeType`).
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// An embedded resource: `{"uri", "mimeType", "text" | "blob"}`.
    Resource {
        /// The resource object.
        resource: Value,
    },
    /// A link to a resource: `{"uri", "name", …}` fields.
    ResourceLink(serde_json::Map<String, Value>),
}

/// Result of a tool call, returned to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// Content returned to the model.
    pub content: Vec<ToolContent>,
    /// Machine-readable output matching `ToolDef::output_schema`.
    pub structured: Option<Value>,
    /// The call failed (`isError`).
    pub is_error: bool,
}

impl ToolResult {
    /// A single text item.
    pub fn text(text: impl Into<String>) -> Self {
        Self::content(vec![ToolContent::Text { text: text.into() }])
    }

    /// A text item marked as an error.
    pub fn error(text: impl Into<String>) -> Self {
        Self { is_error: true, ..Self::text(text) }
    }

    /// A base64 image.
    pub fn image(data: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self::content(vec![ToolContent::Image { data: data.into(), mime_type: mime_type.into() }])
    }

    /// A successful result with these items.
    #[must_use]
    pub fn content(content: Vec<ToolContent>) -> Self {
        Self { content, structured: None, is_error: false }
    }

    /// Strings are passed through verbatim, anything else as JSON text.
    #[must_use]
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::String(s) => Self::text(s),
            other => Self::text(other.to_string()),
        }
    }

    /// Attach structured output (also sent to the model as JSON text when
    /// the result has no content).
    #[must_use]
    pub fn with_structured(mut self, value: Value) -> Self {
        self.structured = Some(value);
        self
    }

    fn into_wire(self) -> Value {
        let content = if self.content.is_empty() {
            let text = self.structured.as_ref().map(Value::to_string).unwrap_or_default();
            vec![ToolContent::Text { text }]
        } else {
            self.content
        };
        let mut body = json!({ "content": content });
        if let Some(s) = self.structured {
            body["structuredContent"] = s;
        }
        if self.is_error {
            body["isError"] = Value::Bool(true);
        }
        body
    }
}

/// What a tool handler knows about its call besides the arguments.
#[derive(Debug, Clone, Default)]
pub struct ToolContext {
    /// The model's `tool_use` id for this call.
    pub tool_use_id: Option<String>,
    /// The session's id: `claudecode/sessionId` in `_meta` when the CLI
    /// sends it (behind a CLI feature flag), else the id the session last
    /// reported. `None` only outside a session (`SdkMcpServer::handle`) or
    /// before the CLI's first `system` frame.
    pub session_id: Option<String>,
    /// The request's raw `_meta` object.
    pub meta: Value,
}

impl ToolContext {
    fn from_params(params: Option<&Value>, session_id: Option<String>) -> Self {
        let meta = params.and_then(|p| p.get("_meta")).cloned().unwrap_or(Value::Null);
        let field = |k: &str| meta.get(k).and_then(Value::as_str).map(str::to_owned);
        Self {
            tool_use_id: field("claudecode/toolUseId"),
            session_id: field("claudecode/sessionId").or(session_id),
            meta,
        }
    }
}

/// A tool definition plus the async function that runs it.
/// `#[claude_tool]` generates `<fn>_tool() -> SdkTool`.
///
/// A call the CLI cancels (`notifications/cancelled`, e.g. after an
/// interrupt) drops the handler's future, so `.await` points are where a
/// handler stops.
#[derive(Clone)]
pub struct SdkTool {
    /// Name, description and schemas sent to the CLI.
    pub def: ToolDef,
    handler: ToolFn,
}

impl fmt::Debug for SdkTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SdkTool").field("name", &self.def.name).finish_non_exhaustive()
    }
}

impl SdkTool {
    /// `handler` receives the raw `arguments` object of the tool call.
    pub fn new<F, Fut>(def: ToolDef, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        Self { def, handler: Arc::new(move |args, _ctx| Box::pin(handler(args))) }
    }

    /// Like `new`, with the call's `ToolContext` (`tool_use` id, session id).
    pub fn with_context<F, Fut>(def: ToolDef, handler: F) -> Self
    where
        F: Fn(Value, ToolContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        Self { def, handler: Arc::new(move |args, ctx| Box::pin(handler(args, ctx))) }
    }
}

/// A named group of SDK tools, served in-process.
#[derive(Debug, Clone)]
pub struct SdkMcpServer {
    /// Server name; tools are exposed as `mcp__<name>__<tool>`.
    pub name: String,
    /// `serverInfo.version` reported to the CLI. Default "1.0.0".
    pub version: String,
    /// Per-call timeout the CLI applies to this server's tools (≥ 1 s);
    /// `None` = the CLI's `MCP_TOOL_TIMEOUT` or its default.
    pub timeout: Option<Duration>,
    tools: Vec<SdkTool>,
    /// Running `tools/call`s by JSON-RPC id; firing one cancels the call.
    /// One map per session (`for_session`): every CLI numbers its requests
    /// from 0, so ids from two sessions collide.
    in_flight: Arc<DashMap<String, oneshot::Sender<()>>>,
}

impl SdkMcpServer {
    /// An empty server named `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: "1.0.0".into(),
            timeout: None,
            tools: vec![],
            in_flight: Arc::new(DashMap::new()),
        }
    }

    /// Add a tool.
    #[must_use]
    pub fn tool(mut self, tool: SdkTool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Set the per-call timeout (`SdkMcpServer::timeout`).
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Fully qualified tool names as Claude sees them: `mcp__<server>__<tool>`.
    #[must_use]
    pub fn qualified_tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| format!("mcp__{}__{}", self.name, t.def.name)).collect()
    }

    /// A copy of this server with its own in-flight call registry, for one
    /// session. Clones share the registry.
    pub(crate) fn for_session(&self) -> Self {
        Self { in_flight: Arc::new(DashMap::new()), ..self.clone() }
    }

    /// This server's entry in an MCP server map (`--mcp-config`, `mcp_set_servers`).
    pub(crate) fn config(&self) -> Value {
        let mut config = json!({ "type": "sdk", "name": self.name });
        if let Some(t) = self.timeout {
            config["timeout"] = json!(u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
        }
        config
    }

    /// Answer one JSON-RPC message from the CLI. Notifications get an empty
    /// result: the control channel answers every `mcp_message`.
    pub async fn handle(&self, message: &Value) -> Value {
        self.handle_in_session(message, None).await
    }

    /// `handle`, with the id of the session the message came from.
    pub(crate) async fn handle_in_session(&self, message: &Value, session_id: Option<String>) -> Value {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params");
        match method {
            "initialize" => rpc_result(id, json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": self.name, "version": self.version },
            })),
            "ping" => rpc_result(id, json!({})),
            "notifications/cancelled" => {
                let request_id = params.and_then(|p| p.get("requestId")).map(Value::to_string);
                if let Some((_, cancel)) = request_id.and_then(|rid| self.in_flight.remove(&rid)) {
                    let _ = cancel.send(());
                }
                json!({ "jsonrpc": "2.0", "result": {} })
            }
            m if m.starts_with("notifications/") => json!({ "jsonrpc": "2.0", "result": {} }),
            "tools/list" => {
                let tools: Vec<Value> = self.tools.iter().map(|t| tool_listing(&t.def)).collect();
                rpc_result(id, json!({ "tools": tools }))
            }
            "tools/call" => self.call_tool(id, params, session_id).await,
            other => rpc_error(id, -32601, &format!("Method '{other}' not found")),
        }
    }

    async fn call_tool(&self, id: Value, params: Option<&Value>, session_id: Option<String>) -> Value {
        let name = params.and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or("");
        let args = params.and_then(|p| p.get("arguments")).cloned().unwrap_or_else(|| json!({}));
        let Some(tool) = self.tools.iter().find(|t| t.def.name == name) else {
            return rpc_error(id, -32602, &format!("Tool '{name}' not found"));
        };
        let key = id.to_string();
        let (cancel_tx, cancel_rx) = oneshot::channel();
        self.in_flight.insert(key.clone(), cancel_tx);
        // Unregisters also when this future is dropped mid-call.
        let _registered = InFlightGuard { in_flight: &self.in_flight, key: &key };
        let run = (tool.handler)(args, ToolContext::from_params(params, session_id));
        let outcome = tokio::select! {
            result = run => Some(result),
            Ok(()) = cancel_rx => None,
        };
        match outcome {
            Some(result) => rpc_result(id, result.into_wire()),
            None => rpc_error(id, REQUEST_CANCELLED, "Request cancelled"),
        }
    }
}

/// Removes a tool call's cancel registration when dropped.
struct InFlightGuard<'a> {
    in_flight: &'a DashMap<String, oneshot::Sender<()>>,
    key: &'a str,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.in_flight.remove(self.key);
    }
}

fn tool_listing(def: &ToolDef) -> Value {
    let mut tool = json!({
        "name": def.name,
        "description": def.description,
        "inputSchema": def.input_schema.clone()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
    });
    if let Some(title) = &def.title {
        tool["title"] = json!(title);
    }
    if let Some(schema) = &def.output_schema {
        tool["outputSchema"] = schema.clone();
    }
    if let Some(annotations) = &def.annotations {
        tool["annotations"] = json!(annotations);
    }
    tool
}

/// An MCP server map with `servers` as SDK entries plus `external` as given.
pub(crate) fn server_map(servers: &[SdkMcpServer], external: Option<&serde_json::Map<String, Value>>) -> Value {
    let mut map: serde_json::Map<String, Value> =
        servers.iter().map(|s| (s.name.clone(), s.config())).collect();
    if let Some(external) = external {
        map.extend(external.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Value::Object(map)
}

/// A JSON-RPC reply; `id` and `result` are moved in, not copied (`json!`
/// would deep-copy a possibly large tool result).
fn rpc_result(id: Value, result: Value) -> Value {
    rpc_reply(id, "result", result)
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    rpc_reply(id, "error", json!({ "code": code, "message": message }))
}

fn rpc_reply(id: Value, key: &str, payload: Value) -> Value {
    let mut reply = serde_json::Map::with_capacity(3);
    reply.insert("jsonrpc".into(), "2.0".into());
    reply.insert("id".into(), id);
    reply.insert(key.into(), payload);
    Value::Object(reply)
}
