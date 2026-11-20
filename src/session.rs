/// Persistent subprocess session — 3-task architecture.
///
///   reader_task   — reads stdout; answers CLI control requests (hooks,
///                   can_use_tool), routes control responses to waiters,
///                   broadcasts everything else
///   writer_task   — drains the stdin channel into the subprocess
///   watchdog_task — waits for exit, marks the session dead, keeps stderr tail
///
/// Wire protocol verified against claude_agent_sdk 0.1.48
/// (_internal/query.py, _internal/transport/subprocess_cli.py) and a live
/// Claude Code 2.1.328 binary.
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::hooks::HookRegistry;
use crate::mcp::{mcp_config_arg, SdkMcpServer};
use crate::protocol::{
    CanUseToolRequest, ControlRequest, ControlRequestBody, ControlResponse,
    InboundControlResponseBody, InboundMessage, Initialize, OutboundControlRequest,
    ToolDecision,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("process exited (code: {code:?}); stderr tail:\n{stderr}")]
    ProcessExited { code: Option<i32>, stderr: String },
    #[error("session closed")]
    Closed,
    #[error("control request `{subtype}` failed: {error}")]
    ControlError { subtype: String, error: String },
    #[error("control request `{0}` timed out")]
    ControlTimeout(String),
}

pub type Result<T> = std::result::Result<T, SessionError>;

const STATE_RUNNING: u8 = 0;
const STATE_CLOSING: u8 = 1;
const STATE_DEAD: u8 = 2;

const STDERR_TAIL_LINES: usize = 40;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);

// ─── Options ─────────────────────────────────────────────────────────────────

type PermFuture = Pin<Box<dyn Future<Output = ToolDecision> + Send + 'static>>;

/// Async tool-permission handler. Setting one makes the CLI ask the host
/// (via `can_use_tool`) whenever its own permission rules would prompt.
#[derive(Clone)]
pub struct CanUseTool(Arc<dyn Fn(CanUseToolRequest) -> PermFuture + Send + Sync>);

impl CanUseTool {
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(CanUseToolRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolDecision> + Send + 'static,
    {
        Self(Arc::new(move |req| Box::pin(f(req))))
    }
}

impl fmt::Debug for CanUseTool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CanUseTool(..)")
    }
}

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub binary: PathBuf,
    /// Working directory of the CLI process. `None` = inherit.
    pub cwd: Option<PathBuf>,
    /// `--model`
    pub model: Option<String>,
    /// `--permission-mode` (default, acceptEdits, plan, bypassPermissions, …)
    pub permission_mode: Option<String>,
    /// `--resume <id>`: continue an existing session's transcript.
    pub resume: Option<String>,
    /// Sets CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1. The stock 2.1.328 binary
    /// also gates Code Mode on internal/experiment checks, so this env var
    /// alone does not enable it there (verified: no REPL/Eval tool appears);
    /// it takes effect on builds where those gates pass.
    /// Source: chunk-0jv1f6sm.codeModeShape.js:13–15
    pub code_mode: bool,
    /// Enable tool search: sets ENABLE_TOOL_SEARCH=1.
    /// Source: chunk-dnmy4w56.js:189
    pub tool_search: bool,
    /// Stream partial chunks (`--include-partial-messages`).
    pub include_partial: bool,
    /// Hook handlers, sent to the CLI in the initialize request.
    pub hooks: HookRegistry,
    /// Tool-permission handler; adds `--permission-prompt-tool stdio`.
    /// Source: subprocess_cli.py:218
    pub can_use_tool: Option<CanUseTool>,
    /// In-process tool servers. Their tools are added to `allowed_tools`
    /// automatically: the host defined them, so asking permission is noise.
    pub mcp_servers: Vec<SdkMcpServer>,
    /// `--allowedTools`: tools that run without a permission prompt.
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools`: tools removed from the model's toolset.
    pub disallowed_tools: Vec<String>,
    /// Write every stdout line to this JSONL file, for `replay::replay`.
    pub record: Option<PathBuf>,
    /// Extra env vars passed to the subprocess.
    pub env: Vec<(String, String)>,
    /// Broadcast channel capacity for inbound events.
    pub event_buffer: usize,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("claude"),
            cwd: None,
            model: None,
            permission_mode: None,
            resume: None,
            code_mode: false,
            tool_search: false,
            include_partial: false,
            hooks: HookRegistry::new(),
            can_use_tool: None,
            mcp_servers: vec![],
            allowed_tools: vec![],
            disallowed_tools: vec![],
            record: None,
            env: vec![],
            event_buffer: 1024,
        }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

pub type EventRx = broadcast::Receiver<Arc<InboundMessage>>;

type Pending = DashMap<String, oneshot::Sender<InboundControlResponseBody>>;

/// How the CLI ended, recorded by the watchdog.
#[derive(Debug, Clone)]
pub(crate) struct ExitInfo {
    pub(crate) code: Option<i32>,
    pub(crate) stderr: String,
}

/// Byte streams of a running CLI — a real subprocess or a replay.
pub(crate) struct Transport {
    pub(crate) stdin: Box<dyn AsyncWrite + Unpin + Send>,
    pub(crate) stdout: Box<dyn AsyncRead + Unpin + Send>,
    /// Resolves when the CLI has exited.
    pub(crate) exit: Pin<Box<dyn Future<Output = ExitInfo> + Send>>,
}

/// A live Claude Code session. Cheap to clone — backed by `Arc<SessionInner>`.
#[derive(Clone)]
pub struct Session(Arc<SessionInner>);

struct SessionInner {
    /// Set from the first `system` frame (every system frame carries it).
    session_id: OnceLock<String>,
    /// `None` after `close()`; dropping the last sender ends the writer task,
    /// which closes the CLI's stdin.
    stdin_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// Weak: the reader task owns the only strong sender, so the channel
    /// closes when the CLI's stdout ends and waiting streams see it.
    event_tx: broadcast::WeakSender<Arc<InboundMessage>>,
    /// Host-initiated control requests awaiting a `control_response`.
    pending: Arc<Pending>,
    next_request_id: AtomicU64,
    state: Arc<AtomicU8>,
    exit: Arc<Mutex<Option<ExitInfo>>>,
    include_partial: bool,
    initialize_response: OnceLock<serde_json::Value>,
    _tasks: [JoinHandle<()>; 3],
}

/// What the reader task needs to answer CLI control requests.
struct Responder {
    /// Weak: only the Session keeps stdin open, so dropping or closing the
    /// Session ends the CLI even while the reader task is still running.
    stdin_tx: mpsc::WeakSender<Vec<u8>>,
    hooks: HookRegistry,
    can_use_tool: Option<CanUseTool>,
    mcp_servers: Vec<SdkMcpServer>,
}

impl Session {
    /// Spawn a Claude Code subprocess and complete the initialize handshake.
    ///
    ///   1. spawn `claude --output-format stream-json --verbose
    ///      --input-format stream-json --await-initialize …`
    ///   2. write the `initialize` control request (with `opts.hooks`)
    ///   3. wait for its `control_response`
    pub async fn spawn(opts: SessionOptions, init: Initialize) -> Result<Self> {
        let mut cmd = Command::new(&opts.binary);
        cmd.args([
            "--output-format", "stream-json",
            "--verbose",
            "--input-format", "stream-json",
            "--await-initialize",
            // Echo each prompt when its turn starts: Agent uses it as the
            // turn boundary. Source: `claude --help`
            "--replay-user-messages",
        ]);
        if opts.include_partial {
            cmd.arg("--include-partial-messages");
        }
        if let Some(ref m) = opts.model {
            cmd.args(["--model", m]);
        }
        if let Some(ref pm) = opts.permission_mode {
            cmd.args(["--permission-mode", pm]);
        }
        if let Some(ref id) = opts.resume {
            cmd.args(["--resume", id]);
        }
        if opts.can_use_tool.is_some() {
            cmd.args(["--permission-prompt-tool", "stdio"]);
        }
        if !opts.mcp_servers.is_empty() {
            cmd.args(["--mcp-config", &mcp_config_arg(&opts.mcp_servers)]);
        }
        let allowed: Vec<String> = opts.allowed_tools.iter().cloned()
            .chain(opts.mcp_servers.iter().flat_map(SdkMcpServer::qualified_tool_names))
            .collect();
        if !allowed.is_empty() {
            cmd.args(["--allowedTools", &allowed.join(",")]);
        }
        if !opts.disallowed_tools.is_empty() {
            cmd.args(["--disallowedTools", &opts.disallowed_tools.join(",")]);
        }
        if let Some(ref cwd) = opts.cwd {
            cmd.current_dir(cwd);
        }
        if opts.code_mode {
            cmd.env("CLAUDE_CODE_EXPERIMENTAL_CODE_MODE", "1");
        }
        if opts.tool_search {
            cmd.env("ENABLE_TOOL_SEARCH", "1");
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false); // closing stdin lets the CLI finish and exit

        let mut child = cmd.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(SessionError::Io(std::io::Error::other("subprocess pipes missing")));
        };
        let exit = Box::pin(async move {
            let mut tail = VecDeque::with_capacity(STDERR_TAIL_LINES);
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            let code = child.wait().await.ok().and_then(|s| s.code());
            ExitInfo { code, stderr: Vec::from(tail).join("\n") }
        });
        let transport = Transport { stdin: Box::new(stdin), stdout: Box::new(stdout), exit };
        Self::connect(transport, opts, init).await
    }

    /// Run the session over an already-started CLI (process or replay) and
    /// complete the initialize handshake.
    pub(crate) async fn connect(transport: Transport, opts: SessionOptions, mut init: Initialize) -> Result<Self> {
        let Transport { stdin, stdout, exit: exit_fut } = transport;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(256);
        let (event_tx, _) = broadcast::channel(opts.event_buffer);
        let pending: Arc<Pending> = Arc::new(DashMap::new());
        let state = Arc::new(AtomicU8::new(STATE_RUNNING));
        let exit = Arc::new(Mutex::new(None));

        let writer = tokio::spawn(async move {
            let mut w = BufWriter::new(stdin);
            while let Some(bytes) = stdin_rx.recv().await {
                if w.write_all(&bytes).await.is_err()
                    || w.write_all(b"\n").await.is_err()
                    || w.flush().await.is_err()
                {
                    break;
                }
            }
            let _ = w.shutdown().await;
        });

        let (exit2, state2) = (exit.clone(), state.clone());
        let watchdog = tokio::spawn(async move {
            let info = exit_fut.await;
            *exit2.lock().await = Some(info);
            state2.store(STATE_DEAD, Ordering::SeqCst);
        });

        let record = match &opts.record {
            Some(path) => Some(tokio::fs::File::create(path).await?),
            None => None,
        };
        let responder = Arc::new(Responder {
            stdin_tx: stdin_tx.downgrade(),
            hooks: opts.hooks.clone(),
            can_use_tool: opts.can_use_tool.clone(),
            mcp_servers: opts.mcp_servers.clone(),
        });
        let event_weak = event_tx.downgrade();
        let reader = tokio::spawn(read_loop(stdout, record, event_tx, pending.clone(), responder));

        let session = Session(Arc::new(SessionInner {
            session_id: OnceLock::new(),
            stdin_tx: Mutex::new(Some(stdin_tx)),
            event_tx: event_weak,
            pending,
            next_request_id: AtomicU64::new(0),
            state,
            exit,
            include_partial: opts.include_partial,
            initialize_response: OnceLock::new(),
            _tasks: [reader, writer, watchdog],
        }));
        session.track_session_id();

        init.hooks = opts.hooks.initialize_config();
        let body = serde_json::to_value(&init)?;
        let body = with_subtype(body, "initialize");
        let response = session.request_with_timeout(&body, INITIALIZE_TIMEOUT).await?;
        let _ = session.0.initialize_response.set(response);
        Ok(session)
    }

    /// Spawn with `--resume <previous_id>`; the CLI reloads its own transcript.
    pub async fn reconnect(previous_id: &str, mut opts: SessionOptions, init: Initialize) -> Result<Self> {
        opts.resume = Some(previous_id.to_string());
        Self::spawn(opts, init).await
    }

    /// Send a raw JSON line to the subprocess stdin.
    pub async fn send<T: serde::Serialize>(&self, msg: &T) -> Result<()> {
        if self.0.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(self.dead_error().await);
        }
        let bytes = serde_json::to_vec(msg)?;
        let tx = self.0.stdin_tx.lock().await.clone().ok_or(SessionError::Closed)?;
        tx.send(bytes).await.map_err(|_| SessionError::Closed)
    }

    /// Send a host → CLI control request and wait for its response payload.
    /// `body` must serialize to an object with a `subtype` field.
    pub async fn control<B: serde::Serialize>(&self, body: &B) -> Result<serde_json::Value> {
        self.request_with_timeout(body, CONTROL_TIMEOUT).await
    }

    /// Interrupt the running turn.
    pub async fn interrupt(&self) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "interrupt" })).await.map(|_| ())
    }

    /// Subscribe to the broadcast event stream.
    pub fn subscribe(&self) -> EventRx {
        match self.0.event_tx.upgrade() {
            Some(tx) => tx.subscribe(),
            // The CLI's stdout has ended: hand out an already-closed receiver.
            None => broadcast::channel(1).1,
        }
    }

    /// Session ID — known once the CLI emits its first `system` frame
    /// (hook events right after spawn, or `system/init` on the first turn).
    pub fn session_id(&self) -> Option<&str> {
        self.0.session_id.get().map(String::as_str)
    }

    /// Payload of the initialize `control_response` (commands, models, account, …).
    pub fn initialize_response(&self) -> Option<&serde_json::Value> {
        self.0.initialize_response.get()
    }

    pub fn include_partial(&self) -> bool {
        self.0.include_partial
    }

    /// Whether the subprocess is still running.
    pub fn is_alive(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == STATE_RUNNING
    }

    /// Close stdin and wait (up to 30 s) for the CLI to finish and exit.
    /// Returns the exit code once the process has exited.
    /// Source: subprocess_cli.py:517–521 (end_input)
    pub async fn close(&self) -> Option<i32> {
        if self.0.state.load(Ordering::Acquire) == STATE_RUNNING {
            self.0.state.store(STATE_CLOSING, Ordering::SeqCst);
        }
        self.0.stdin_tx.lock().await.take();
        for _ in 0..300 {
            if let Some(info) = self.0.exit.lock().await.as_ref() {
                return info.code;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        tracing::warn!("CLI did not exit within 30 s of closing stdin");
        None
    }

    // ── internals ────────────────────────────────────────────────────────────

    async fn request_with_timeout<B: serde::Serialize>(
        &self,
        body: &B,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        let n = self.0.next_request_id.fetch_add(1, Ordering::Relaxed);
        let request_id = format!("req_{n}_{}", std::process::id());
        let subtype = serde_json::to_value(body)?
            .get("subtype")
            .and_then(|s| s.as_str())
            .unwrap_or("?")
            .to_string();

        // Register before writing so a fast response cannot arrive unmatched.
        let (tx, rx) = oneshot::channel();
        self.0.pending.insert(request_id.clone(), tx);
        let envelope = OutboundControlRequest { kind: "control_request", request_id: &request_id, request: body };
        if let Err(e) = self.send(&envelope).await {
            self.0.pending.remove(&request_id);
            return Err(e);
        }

        let reply = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => reply,
            // Sender dropped: the reader hit EOF because the process exited.
            Ok(Err(_)) => return Err(self.dead_error().await),
            Err(_) => {
                self.0.pending.remove(&request_id);
                return Err(SessionError::ControlTimeout(subtype));
            }
        };
        match reply.subtype.as_str() {
            "success" => Ok(reply.response.unwrap_or(serde_json::Value::Null)),
            _ => Err(SessionError::ControlError {
                subtype,
                error: reply.error.unwrap_or_else(|| "unknown error".into()),
            }),
        }
    }

    /// Error describing why the process is gone, with its stderr tail.
    async fn dead_error(&self) -> SessionError {
        // The watchdog records the exit right after stdout/stderr close.
        for _ in 0..50 {
            if let Some(ExitInfo { code, stderr }) = self.0.exit.lock().await.clone() {
                return SessionError::ProcessExited { code, stderr };
            }
            if self.0.state.load(Ordering::Acquire) == STATE_CLOSING {
                return SessionError::Closed;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        SessionError::Closed
    }

    fn track_session_id(&self) {
        let mut rx = self.subscribe();
        let inner = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(msg) => {
                        if let InboundMessage::System(ref s) = *msg {
                            if let Some(inner) = inner.upgrade() {
                                let _ = inner.session_id.set(s.session_id.clone());
                            }
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
}

fn with_subtype(mut body: serde_json::Value, subtype: &str) -> serde_json::Value {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("subtype".into(), subtype.into());
    }
    body
}

async fn read_loop(
    stdout: Box<dyn AsyncRead + Unpin + Send>,
    mut record: Option<tokio::fs::File>,
    event_tx: broadcast::Sender<Arc<InboundMessage>>,
    pending: Arc<Pending>,
    responder: Arc<Responder>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(file) = record.as_mut() {
            if file.write_all(line.as_bytes()).await.is_err() || file.write_all(b"\n").await.is_err() {
                tracing::warn!("recording failed; continuing without it");
                record = None;
            }
        }
        let msg: InboundMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!(error = %e, line = %line, "unparsed stdout line");
                InboundMessage::Unknown
            }
        };
        match msg {
            InboundMessage::ControlResponse(r) => {
                if let Some((_, tx)) = pending.remove(&r.response.request_id) {
                    let _ = tx.send(r.response);
                }
            }
            InboundMessage::ControlRequest(req) => {
                // Answer without blocking the reader: hooks may run long.
                let responder = responder.clone();
                tokio::spawn(async move { responder.answer(req).await });
            }
            other => {
                let _ = event_tx.send(Arc::new(other));
            }
        }
    }
    // EOF: drop every waiter so it reports the exit instead of hanging.
    pending.clear();
}

impl Responder {
    async fn answer(&self, req: ControlRequest) {
        let response = match req.request {
            ControlRequestBody::HookCallback(hcb) => {
                let span = tracing::info_span!(
                    "claude.hook",
                    callback_id = %hcb.callback_id,
                    event = ?hcb.input.hook_event_name,
                    tool = hcb.input.tool_name.as_deref().unwrap_or(""),
                    blocked = tracing::field::Empty,
                );
                let output = self.hooks.dispatch(&hcb.callback_id, hcb.input).instrument(span.clone()).await;
                if let Some(ref o) = output {
                    span.record("blocked", o.is_block());
                }
                match output {
                    Some(output) => match serde_json::to_value(output) {
                        Ok(v) => ControlResponse::success(&req.request_id, v),
                        Err(e) => ControlResponse::error(&req.request_id, e.to_string()),
                    },
                    None => ControlResponse::error(
                        &req.request_id,
                        format!("no hook callback registered for id {}", hcb.callback_id),
                    ),
                }
            }
            ControlRequestBody::CanUseTool(cut) => match &self.can_use_tool {
                Some(handler) => {
                    let original = cut.input.clone();
                    let decision = match (handler.0)(cut).await {
                        // The CLI expects updatedInput on allow; echo the original.
                        ToolDecision::Allow { updated_input } => ToolDecision::Allow {
                            updated_input: Some(updated_input.unwrap_or(original)),
                        },
                        deny => deny,
                    };
                    match serde_json::to_value(decision) {
                        Ok(v) => ControlResponse::success(&req.request_id, v),
                        Err(e) => ControlResponse::error(&req.request_id, e.to_string()),
                    }
                }
                None => ControlResponse::error(&req.request_id, "no can_use_tool handler"),
            },
            ControlRequestBody::McpMessage(m) => {
                let span = tracing::info_span!(
                    "claude.tool",
                    server = %m.server_name,
                    method = m.message.get("method").and_then(|v| v.as_str()).unwrap_or(""),
                    tool = m.message.pointer("/params/name").and_then(|v| v.as_str()).unwrap_or(""),
                );
                let reply = match self.mcp_servers.iter().find(|s| s.name == m.server_name) {
                    Some(server) => server.handle(&m.message).instrument(span).await,
                    None => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": m.message.get("id"),
                        "error": { "code": -32601, "message": format!("Server '{}' not found", m.server_name) },
                    }),
                };
                ControlResponse::success(&req.request_id, serde_json::json!({ "mcp_response": reply }))
            }
            ControlRequestBody::Unsupported => {
                ControlResponse::error(&req.request_id, "unsupported control request subtype")
            }
        };
        match serde_json::to_vec(&response) {
            Ok(bytes) => match self.stdin_tx.upgrade() {
                Some(tx) => {
                    let _ = tx.send(bytes).await;
                }
                None => tracing::debug!("session closed; dropping control response"),
            },
            Err(e) => tracing::warn!(error = %e, "failed to serialize control response"),
        }
    }
}
