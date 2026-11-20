/// In-process (SDK) MCP servers — Rust functions exposed to Claude as tools.
///
/// The CLI is told about each server with
/// `--mcp-config {"mcpServers":{"<name>":{"type":"sdk","name":"<name>"}}}` and
/// then forwards every JSON-RPC message for it as an `mcp_message` control
/// request; the session answers with `{"mcp_response": <JSON-RPC response>}`.
/// Claude sees the tools as `mcp__<server>__<tool>`.
///
/// Source: claude_agent_sdk 0.1.48 transport/subprocess_cli.py:240–260,
///         _internal/query.py:305–320, :394–500
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::protocol::ToolDef;

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type ToolFn = Arc<dyn Fn(Value) -> BoxFuture<ToolResult> + Send + Sync>;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Result of a tool call, returned to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true }
    }

    /// Strings are passed through verbatim, anything else as JSON.
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::String(s) => Self::text(s),
            other => Self::text(other.to_string()),
        }
    }
}

/// A tool definition plus the async function that runs it.
/// `#[claude_tool]` generates `<fn>_tool() -> SdkTool`.
#[derive(Clone)]
pub struct SdkTool {
    pub def: ToolDef,
    handler: ToolFn,
}

impl fmt::Debug for SdkTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SdkTool").field("name", &self.def.name).finish()
    }
}

impl SdkTool {
    /// `handler` receives the raw `arguments` object of the tool call.
    pub fn new<F, Fut>(def: ToolDef, handler: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        Self { def, handler: Arc::new(move |args| Box::pin(handler(args))) }
    }
}

/// A named group of SDK tools, served in-process.
#[derive(Debug, Clone)]
pub struct SdkMcpServer {
    pub name: String,
    pub version: String,
    tools: Vec<SdkTool>,
}

impl SdkMcpServer {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), version: "1.0.0".into(), tools: vec![] }
    }

    pub fn tool(mut self, tool: SdkTool) -> Self {
        self.tools.push(tool);
        self
    }

    /// Fully qualified tool names as Claude sees them: `mcp__<server>__<tool>`.
    pub fn qualified_tool_names(&self) -> Vec<String> {
        self.tools.iter().map(|t| format!("mcp__{}__{}", self.name, t.def.name)).collect()
    }

    /// Answer one JSON-RPC message from the CLI.
    pub async fn handle(&self, message: &Value) -> Value {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        match method {
            "initialize" => rpc_result(id, json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": self.name, "version": self.version },
            })),
            "notifications/initialized" => json!({ "jsonrpc": "2.0", "result": {} }),
            "tools/list" => {
                let tools: Vec<Value> = self.tools.iter().map(|t| json!({
                    "name": t.def.name,
                    "description": t.def.description,
                    "inputSchema": t.def.input_schema.clone()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })).collect();
                rpc_result(id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let params = message.get("params");
                let name = params.and_then(|p| p.get("name")).and_then(Value::as_str).unwrap_or("");
                let args = params
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let Some(tool) = self.tools.iter().find(|t| t.def.name == name) else {
                    return rpc_error(id, -32602, format!("Tool '{name}' not found"));
                };
                let result = (tool.handler)(args).await;
                let mut body = json!({ "content": [{ "type": "text", "text": result.text }] });
                if result.is_error {
                    body["isError"] = Value::Bool(true);
                }
                rpc_result(id, body)
            }
            other => rpc_error(id, -32601, format!("Method '{other}' not found")),
        }
    }
}

/// The `--mcp-config` argument for a set of SDK servers.
pub(crate) fn mcp_config_arg(servers: &[SdkMcpServer]) -> String {
    let map: serde_json::Map<String, Value> = servers
        .iter()
        .map(|s| (s.name.clone(), json!({ "type": "sdk", "name": s.name })))
        .collect();
    json!({ "mcpServers": map }).to_string()
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
