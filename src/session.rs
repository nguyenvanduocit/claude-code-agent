//! Persistent subprocess session — 3-task architecture.
//!
//!   reader_task   — reads stdout; answers CLI control requests (hooks,
//!                   can_use_tool, mcp_message), routes control responses to
//!                   waiters, fans every other frame out to all subscribers
//!                   (one unbounded channel each: no frame is ever dropped)
//!   writer_task   — drains the stdin channel into the subprocess
//!   watchdog_task — waits for exit, marks the session dead, keeps stderr tail
//!
//! Wire protocol verified against claude_agent_sdk 0.1.48
//! (_internal/query.py, _internal/transport/subprocess_cli.py) and a live
//! Claude Code 2.1.328 binary.
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::hooks::HookRegistry;
use crate::mcp::{mcp_config_arg, SdkMcpServer};
use crate::protocol::{
    CanUseToolRequest, ControlRequest, ControlRequestBody, ControlResponse,
    InboundControlResponseBody, InboundMessage, Initialize, OutboundControlRequest, ResultMessage,
    PermissionMode, ToolDecision,
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
    #[error("code mode: {0}")]
    CodeMode(#[from] crate::code_mode::CodeModeError),
}

pub type Result<T> = std::result::Result<T, SessionError>;

const STATE_RUNNING: u8 = 0;
const STATE_CLOSING: u8 = 1;
const STATE_DEAD: u8 = 2;

const STDERR_TAIL_LINES: usize = 40;
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
/// How long `close()` waits after closing stdin, and after each signal.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

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
    /// `--permission-mode`
    pub permission_mode: Option<PermissionMode>,
    /// `--resume <id>`: continue an existing session's transcript.
    pub resume: Option<String>,
    /// Force Code Mode on: Claude gets the `REPL` tool and works by writing
    /// JavaScript that calls its tools. The stock CLI refuses Code Mode for
    /// SDK sessions, so this runs a patched copy of `binary`, built once and
    /// cached (see `code_mode` module); the installed CLI is not modified.
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
    /// `--setting-sources`: which settings files to load ("user", "project",
    /// "local"). `Some(vec![])` loads none — the session ignores the user's
    /// global CLAUDE.md, skills, permission rules and hooks, and turns get
    /// much cheaper. `None` = CLI default (all).
    /// Source: subprocess_cli.py:275–280
    pub setting_sources: Option<Vec<String>>,
    /// `--settings`: inline settings object (permissions, env, shell `hooks`, …),
    /// or a string: path of a settings file (relative to `cwd`). With
    /// `code_mode` the file is read and passed inline with the Code Mode env var.
    pub settings: Option<serde_json::Value>,
    /// Write every stdout line to this JSONL file, for `replay::replay`.
    pub record: Option<PathBuf>,
    /// Start the CLI with an empty environment plus `env` only. The CLI then
    /// needs at least HOME and PATH (and credentials) passed through `env`.
    /// `false` (default) inherits the parent environment, like the official SDKs.
    pub clear_env: bool,
    /// Extra env vars passed to the subprocess.
    pub env: Vec<(String, String)>,
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
            setting_sources: None,
            settings: None,
            record: None,
            clear_env: false,
            env: vec![],
        }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

/// Receives every frame the CLI emits after `subscribe()`; `None` once the
/// CLI's stdout has ended.
pub type EventRx = mpsc::UnboundedReceiver<Arc<InboundMessage>>;

/// Subscribers of the reader task. `ended` turns new subscriptions into
/// already-closed receivers once stdout is gone.
#[derive(Default)]
struct Fanout {
    subs: Vec<mpsc::UnboundedSender<Arc<InboundMessage>>>,
    ended: bool,
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    /// Process group of the CLI and everything it started (Bash tools, …).
    pub(crate) pgid: Option<i32>,
}

/// A live Claude Code session. Cheap to clone — backed by `Arc<SessionInner>`.
#[derive(Clone)]
pub struct Session(Arc<SessionInner>);

struct SessionInner {
    /// Current session id; every `system` frame carries it and `/clear`
    /// starts a new one.
    session_id: Arc<RwLock<Option<String>>>,
    /// `None` after `close()`; dropping the last sender ends the writer task,
    /// which closes the CLI's stdin.
    stdin_tx: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    fanout: Arc<std::sync::Mutex<Fanout>>,
    /// Host-initiated control requests awaiting a `control_response`.
    pending: Arc<Pending>,
    next_request_id: AtomicU64,
    state: Arc<AtomicU8>,
    exit: watch::Receiver<Option<ExitInfo>>,
    pgid: Option<i32>,
    include_partial: bool,
    initialize_response: OnceLock<serde_json::Value>,
    /// Prompts sent whose `result` has not arrived yet.
    turns_in_flight: Arc<AtomicUsize>,
    /// `true` once the reader has handled the last stdout line.
    reader_done: watch::Receiver<bool>,
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
        let binary = if opts.code_mode {
            crate::code_mode::patched_binary(&opts.binary).await?
        } else {
            opts.binary.clone()
        };
        let mut cmd = Command::new(&binary);
        // First, so the variables set below survive it.
        if opts.clear_env {
            cmd.env_clear();
        }
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
        if let Some(pm) = opts.permission_mode {
            cmd.args(["--permission-mode", pm.as_str()]);
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
        if let Some(ref sources) = opts.setting_sources {
            cmd.args(["--setting-sources", &sources.join(",")]);
        }
        let settings = match (&opts.settings, opts.code_mode) {
            // The Code Mode env var must be merged into the settings object.
            (Some(serde_json::Value::String(path)), true) => {
                Some(read_settings_file(path, opts.cwd.as_deref()).await?)
            }
            (settings, _) => settings.clone(),
        };
        match settings_arg(settings, opts.code_mode)? {
            // A string is a settings file path; anything else is inline JSON.
            Some(serde_json::Value::String(path)) => {
                cmd.args(["--settings", &path]);
            }
            Some(settings) => {
                cmd.args(["--settings", &settings.to_string()]);
            }
            None => {}
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
            .kill_on_drop(false); // close() / Drop end the whole process group instead
        // Own process group: tools the CLI starts (Bash, background tasks)
        // join it, so close() and Drop can end them too.
        #[cfg(unix)]
        cmd.process_group(0);

        // Before spawning, so a bad path leaves no process behind.
        let record = open_record(&opts).await?;
        let mut child = cmd.spawn()?;
        let pgid = child.id().and_then(|id| i32::try_from(id).ok());
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
        let transport = Transport { stdin: Box::new(stdin), stdout: Box::new(stdout), exit, pgid };
        Self::connect(transport, opts, init, record).await
    }

    /// Run the session over an already-started CLI (process or replay) and
    /// complete the initialize handshake.
    pub(crate) async fn connect(
        transport: Transport,
        opts: SessionOptions,
        mut init: Initialize,
        record: Option<tokio::fs::File>,
    ) -> Result<Self> {
        let Transport { stdin, stdout, exit: exit_fut, pgid } = transport;
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(256);
        let fanout = Arc::new(std::sync::Mutex::new(Fanout::default()));
        let session_id = Arc::new(RwLock::new(None));
        let pending: Arc<Pending> = Arc::new(DashMap::new());
        let state = Arc::new(AtomicU8::new(STATE_RUNNING));
        let (exit_tx, exit_rx) = watch::channel(None);
        let (reader_done_tx, reader_done) = watch::channel(false);
        let turns_in_flight = Arc::new(AtomicUsize::new(0));

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

        let state2 = state.clone();
        let watchdog = tokio::spawn(async move {
            let info = exit_fut.await;
            state2.store(STATE_DEAD, Ordering::SeqCst);
            let _ = exit_tx.send(Some(info));
        });

        let responder = Arc::new(Responder {
            stdin_tx: stdin_tx.downgrade(),
            hooks: opts.hooks.clone(),
            can_use_tool: opts.can_use_tool.clone(),
            mcp_servers: opts.mcp_servers.clone(),
        });
        let reader = tokio::spawn(read_loop(stdout, Reader {
            record,
            fanout: fanout.clone(),
            session_id: session_id.clone(),
            pending: pending.clone(),
            turns_in_flight: turns_in_flight.clone(),
            responder,
            done: reader_done_tx,
        }));

        let session = Session(Arc::new(SessionInner {
            session_id,
            stdin_tx: Mutex::new(Some(stdin_tx)),
            fanout,
            pending,
            next_request_id: AtomicU64::new(0),
            state,
            exit: exit_rx,
            pgid,
            include_partial: opts.include_partial,
            initialize_response: OnceLock::new(),
            turns_in_flight,
            reader_done,
            _tasks: [reader, writer, watchdog],
        }));

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

    /// Interrupt the running turn; its stream ends with
    /// an `AgentEvent::Error` starting with `"error_during_execution"`.
    pub async fn interrupt(&self) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "interrupt" })).await.map(|_| ())
    }

    /// Switch model for the following turns; `None` restores the session default.
    /// Source: binary set_model schema
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "set_model", "model": model })).await.map(|_| ())
    }

    /// Source: binary set_permission_mode schema
    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "set_permission_mode", "mode": mode })).await.map(|_| ())
    }

    /// Stop a background task by its `task_id` (see `AgentEvent::BackgroundTasks`).
    /// Source: binary stop_task schema
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "stop_task", "task_id": task_id })).await.map(|_| ())
    }

    /// Context-window usage of the session, as reported by the CLI.
    /// Source: binary get_context_usage schema
    pub async fn context_usage(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_context_usage" })).await
    }

    /// Receive every frame the CLI emits from now on. Lossless: a slow
    /// receiver buffers instead of dropping frames.
    pub fn subscribe(&self) -> EventRx {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut fanout = lock(&self.0.fanout);
        if !fanout.ended {
            fanout.subs.push(tx);
        }
        rx
    }

    /// Current session id — known once the CLI emits its first `system`
    /// frame; changes after `/clear`.
    pub fn session_id(&self) -> Option<String> {
        self.0.session_id.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    /// Payload of the initialize `control_response` (commands, models, account, …).
    pub fn initialize_response(&self) -> Option<&serde_json::Value> {
        self.0.initialize_response.get()
    }

    pub fn include_partial(&self) -> bool {
        self.0.include_partial
    }

    /// Whether a prompt sent with `Agent` is still waiting for its `result`.
    pub fn is_busy(&self) -> bool {
        self.0.turns_in_flight.load(Ordering::Acquire) > 0
    }

    /// Send a prompt and count it as in flight until its `result`. Counted
    /// before the write, so a fast `result` cannot arrive first.
    pub(crate) async fn send_prompt(&self, message: &crate::protocol::UserMessage) -> Result<()> {
        self.0.turns_in_flight.fetch_add(1, Ordering::AcqRel);
        let sent = self.send(message).await;
        if sent.is_err() {
            decrement(&self.0.turns_in_flight);
        }
        sent
    }

    /// Whether the subprocess is still running.
    pub fn is_alive(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == STATE_RUNNING
    }

    /// End the session: close stdin and give the CLI 5 s to exit, then
    /// SIGTERM, then SIGKILL its process group. Tools it started (background
    /// Bash commands keep the CLI alive past stdin EOF) end with it.
    /// Returns the CLI's exit code (`None` when it ended by a signal).
    pub async fn close(&self) -> Option<i32> {
        let _ = self.0.state.compare_exchange(STATE_RUNNING, STATE_CLOSING, Ordering::SeqCst, Ordering::SeqCst);
        self.0.stdin_tx.lock().await.take();
        let mut exit = self.wait_exit(CLOSE_GRACE).await;
        #[cfg(unix)]
        for signal in [nix::sys::signal::Signal::SIGTERM, nix::sys::signal::Signal::SIGKILL] {
            if exit.is_some() {
                break;
            }
            self.signal_group(signal);
            exit = self.wait_exit(CLOSE_GRACE).await;
        }
        // Children the CLI left running share its process group.
        #[cfg(unix)]
        self.signal_group(nix::sys::signal::Signal::SIGTERM);
        // Let the reader finish the last lines (and the recording).
        let mut done = self.0.reader_done.clone();
        let _ = tokio::time::timeout(CLOSE_GRACE, done.wait_for(|d| *d)).await;
        exit.and_then(|e| e.code)
    }

    async fn wait_exit(&self, timeout: Duration) -> Option<ExitInfo> {
        let mut rx = self.0.exit.clone();
        let waited = tokio::time::timeout(timeout, async {
            rx.wait_for(Option::is_some).await.ok().and_then(|info| info.clone())
        })
        .await;
        waited.ok().flatten()
    }

    #[cfg(unix)]
    fn signal_group(&self, signal: nix::sys::signal::Signal) {
        if let Some(pgid) = self.0.pgid {
            let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), signal);
        }
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
        // The reader marks `ended` before it drops all waiters; a request
        // registered after that would never be answered.
        if lock(&self.0.fanout).ended {
            self.0.pending.remove(&request_id);
            return Err(self.dead_error().await);
        }
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
        if self.0.state.load(Ordering::Acquire) == STATE_CLOSING {
            return SessionError::Closed;
        }
        // The watchdog records the exit right after stdout/stderr close.
        match self.wait_exit(Duration::from_secs(1)).await {
            Some(ExitInfo { code, stderr }) => SessionError::ProcessExited { code, stderr },
            None => SessionError::Closed,
        }
    }
}

impl Drop for SessionInner {
    /// Dropping the last handle ends the CLI and the tools it started.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid
            && self.state.load(Ordering::Acquire) != STATE_DEAD {
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pgid), nix::sys::signal::Signal::SIGTERM);
            }
    }
}

/// Decrement, stopping at zero.
fn decrement(n: &AtomicUsize) {
    let mut current = n.load(Ordering::Acquire);
    while current > 0 {
        match n.compare_exchange_weak(current, current - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

/// The file `SessionOptions::record` names, created (truncated) for writing.
pub(crate) async fn open_record(opts: &SessionOptions) -> Result<Option<tokio::fs::File>> {
    match &opts.record {
        Some(path) => Ok(Some(tokio::fs::File::create(path).await?)),
        None => Ok(None),
    }
}

/// Settings file contents; a relative path is resolved against `cwd`, as
/// the CLI started in `cwd` would.
async fn read_settings_file(path: &str, cwd: Option<&std::path::Path>) -> Result<serde_json::Value> {
    let path = match cwd {
        Some(dir) => dir.join(path),
        None => PathBuf::from(path),
    };
    Ok(serde_json::from_slice(&tokio::fs::read(path).await?)?)
}

/// `--settings` value: the caller's settings, plus the Code Mode env var,
/// which the CLI honours only when it comes from settings, not the shell.
fn settings_arg(settings: Option<serde_json::Value>, code_mode: bool) -> Result<Option<serde_json::Value>> {
    if !code_mode {
        return Ok(settings);
    }
    let mut settings = settings.unwrap_or_else(|| serde_json::json!({}));
    let env = settings
        .as_object_mut()
        .map(|obj| obj.entry("env").or_insert_with(|| serde_json::json!({})))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(crate::code_mode::CodeModeError::Settings)?;
    env.insert("CLAUDE_CODE_EXPERIMENTAL_CODE_MODE".into(), "1".into());
    Ok(Some(settings))
}

fn with_subtype(mut body: serde_json::Value, subtype: &str) -> serde_json::Value {
    if let Some(obj) = body.as_object_mut() {
        obj.insert("subtype".into(), subtype.into());
    }
    body
}

/// State the reader task shares with the Session.
struct Reader {
    record: Option<tokio::fs::File>,
    fanout: Arc<std::sync::Mutex<Fanout>>,
    session_id: Arc<RwLock<Option<String>>>,
    pending: Arc<Pending>,
    turns_in_flight: Arc<AtomicUsize>,
    responder: Arc<Responder>,
    done: watch::Sender<bool>,
}

async fn read_loop(stdout: Box<dyn AsyncRead + Unpin + Send>, reader: Reader) {
    let Reader { mut record, fanout, session_id, pending, turns_in_flight, responder, done: reader_done } = reader;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(file) = record.as_mut()
            && (file.write_all(line.as_bytes()).await.is_err()
                || file.write_all(b"\n").await.is_err()
                || file.flush().await.is_err())
        {
            tracing::warn!("recording failed; continuing without it");
            record = None;
        }
        let msg = parse_frame(&line);
        match msg {
            InboundMessage::ControlResponse(r) => {
                if let Some((_, tx)) = pending.remove(&r.response.request_id) {
                    let _ = tx.send(r.response);
                }
            }
            InboundMessage::ControlRequest(req) => {
                // Answer without blocking the reader: hooks may run long.
                // The handler runs in its own task, so a panic in user code
                // still sends an error response instead of leaving the CLI
                // waiting.
                let responder = responder.clone();
                tokio::spawn(async move {
                    let request_id = req.request_id.clone();
                    let handler = responder.clone();
                    let response = match tokio::spawn(async move { handler.respond(req).await }).await {
                        Ok(response) => response,
                        Err(e) => ControlResponse::error(&request_id, format!("handler failed: {e}")),
                    };
                    responder.write(&response).await;
                });
            }
            other => {
                if let InboundMessage::Result(ref r) = other
                    && r.origin.is_none()
                {
                    decrement(&turns_in_flight);
                }
                if let InboundMessage::System(ref s) = other {
                    let mut id = session_id.write().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if id.as_deref() != Some(s.session_id.as_str()) {
                        *id = Some(s.session_id.clone());
                    }
                }
                let msg = Arc::new(other);
                lock(&fanout).subs.retain(|tx| tx.send(msg.clone()).is_ok());
            }
        }
    }
    // EOF: close every subscriber and drop every waiter, so streams and
    // control requests report the exit instead of hanging.
    {
        let mut f = lock(&fanout);
        f.ended = true;
        f.subs.clear();
    }
    pending.clear();
    turns_in_flight.store(0, Ordering::Release);
    drop(record);
    let _ = reader_done.send(true);
}

/// Decode one stdout line. A `result` the typed model cannot decode still
/// ends its turn, as a minimal result, so no stream waits forever.
fn parse_frame(line: &str) -> InboundMessage {
    match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => {
            let value: Option<serde_json::Value> = serde_json::from_str(line).ok();
            let field = |k: &str| value.as_ref().and_then(|v| v.get(k)).and_then(|v| v.as_str()).map(str::to_owned);
            if field("type").as_deref() == Some("result") {
                tracing::warn!(error = %e, "undecodable result frame; ending the turn with it");
                InboundMessage::Result(ResultMessage {
                    subtype: field("subtype").unwrap_or_else(|| "error_unparsed_result".into()),
                    is_error: value.as_ref().and_then(|v| v.get("is_error")).and_then(|v| v.as_bool()).unwrap_or(false),
                    result: field("result"),
                    session_id: field("session_id"),
                    origin: value.as_ref().and_then(|v| v.get("origin")).cloned(),
                    ..ResultMessage::default()
                })
            } else if let (Some("control_request"), Some(request_id)) = (field("type").as_deref(), value.as_ref().and_then(|v| v.get("request_id")).and_then(|v| v.as_str())) {
                // The CLI waits for an answer; an error reply beats a hang.
                tracing::warn!(error = %e, "undecodable control_request; answering with an error");
                InboundMessage::ControlRequest(ControlRequest {
                    request_id: request_id.to_owned(),
                    request: ControlRequestBody::Unsupported,
                })
            } else {
                tracing::debug!(error = %e, line = %line, "unparsed stdout line");
                InboundMessage::Unknown
            }
        }
    }
}

impl Responder {
    async fn respond(&self, req: ControlRequest) -> ControlResponse {
        match req.request {
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
        }
    }

    async fn write(&self, response: &ControlResponse) {
        match serde_json::to_vec(response) {
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
