//! Persistent subprocess session — 3-task architecture.
//!
//!   `reader_task`   — reads stdout; answers CLI control requests (hooks,
//!                   `can_use_tool`, `mcp_message`), routes control responses to
//!                   waiters, fans every other frame out to all subscribers
//!                   (one unbounded channel each: no frame is ever dropped)
//!   `writer_task`   — drains the stdin channel into the subprocess
//!   `watchdog_task` — waits for exit, marks the session dead, keeps stderr tail
//!
//! Wire protocol verified against `claude_agent_sdk` 0.1.48
//! (_internal/query.py, _`internal/transport/subprocess_cli.py`) and a live
//! Claude Code 2.1.329 binary.
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
use crate::mcp::{server_map, SdkMcpServer};
use crate::protocol::{
    Autocompact, CanUseToolRequest, ControlRequest, ControlRequestBody, ControlResponse, Effort,
    ElicitationRequest, ElicitationResponse, InboundControlResponseBody, InboundMessage, Initialize,
    OutboundControlRequest, PermissionMode, PermissionPrompts, ResultMessage, Thinking, ThinkingDisplay,
    ToolDecision,
    UserDialogRequest, UserDialogResponse,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Error of a session operation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// An I/O error, e.g. spawning the CLI or opening a recording file.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A frame could not be serialized or parsed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// The CLI process exited.
    #[error("process exited (code: {code:?}); stderr tail:\n{stderr}")]
    ProcessExited {
        /// Exit code; `None` when killed by a signal.
        code: Option<i32>,
        /// Last lines the CLI wrote to stderr.
        stderr: String,
    },
    /// The session was closed, or stdin is gone.
    #[error("session closed")]
    Closed,
    /// The CLI answered a control request with an error.
    #[error("control request `{subtype}` failed: {error}")]
    ControlError {
        /// The request's `subtype`.
        subtype: String,
        /// The CLI's error message.
        error: String,
    },
    /// No answer to the control request (named by its `subtype`) in time
    /// (`SessionOptions::control_timeout` for requests after initialize).
    #[error("control request `{0}` timed out")]
    ControlTimeout(String),
    /// Building the Code Mode binary failed.
    #[error("code mode: {0}")]
    CodeMode(#[from] crate::code_mode::CodeModeError),
    /// The CLI is older than `MIN_CLI_VERSION`.
    #[error("Claude Code {found} is older than {min}, the oldest version this SDK supports; run `claude update`")]
    UnsupportedCliVersion {
        /// Version the CLI reported.
        found: String,
        /// `MIN_CLI_VERSION`.
        min: &'static str,
    },
    /// A recording passed to `replay` that cannot be replayed.
    #[error("invalid recording: {0}")]
    InvalidRecording(String),
}

/// `Result` with `SessionError`.
pub type Result<T> = std::result::Result<T, SessionError>;

const STATE_RUNNING: u8 = 0;
const STATE_CLOSING: u8 = 1;
const STATE_DEAD: u8 = 2;

const STDERR_TAIL_LINES: usize = 40;
/// After the CLI exits, how long stderr may still deliver its last lines.
const STDERR_LINGER: Duration = Duration::from_millis(500);
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Default of `SessionOptions::control_timeout`.
pub const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
/// Oldest Claude Code version the wire protocol here is verified against.
pub const MIN_CLI_VERSION: &str = "2.1.329";
/// How long `close()` waits after closing stdin, and after each signal.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

// ─── Options ─────────────────────────────────────────────────────────────────

type PermFuture = Pin<Box<dyn Future<Output = ToolDecision> + Send + 'static>>;

/// Async tool-permission handler. Setting one makes the CLI ask the host
/// (via `can_use_tool`) whenever its own permission rules would prompt.
#[derive(Clone)]
pub struct CanUseTool(Arc<dyn Fn(CanUseToolRequest) -> PermFuture + Send + Sync>);

impl CanUseTool {
    /// Wrap an async handler.
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(CanUseToolRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolDecision> + Send + 'static,
    {
        Self(Arc::new(move |req| Box::pin(f(req))))
    }
}

type ElicitFuture = Pin<Box<dyn Future<Output = ElicitationResponse> + Send + 'static>>;

/// Answers MCP elicitations (an MCP server asking the user for input).
/// Without one, the CLI is told the request is unsupported.
#[derive(Clone)]
pub struct OnElicitation(Arc<dyn Fn(ElicitationRequest) -> ElicitFuture + Send + Sync>);

impl OnElicitation {
    /// Wrap an async handler.
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(ElicitationRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ElicitationResponse> + Send + 'static,
    {
        Self(Arc::new(move |req| Box::pin(f(req))))
    }
}

type DialogFuture = Pin<Box<dyn Future<Output = UserDialogResponse> + Send + 'static>>;

/// Renders `request_user_dialog` dialogs and returns the user's choice,
/// e.g. `UserDialogResponse::completed("retry_fallback")` for a
/// `refusal_fallback_prompt`. List the kinds it handles in
/// `Initialize::supported_dialog_kinds`; the CLI sends no other kind.
#[derive(Clone)]
pub struct OnUserDialog(Arc<dyn Fn(UserDialogRequest) -> DialogFuture + Send + Sync>);

impl OnUserDialog {
    /// Wrap an async handler.
    pub fn new<F, Fut>(f: F) -> Self
    where
        F: Fn(UserDialogRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = UserDialogResponse> + Send + 'static,
    {
        Self(Arc::new(move |req| Box::pin(f(req))))
    }
}

/// Receives every stderr line of the CLI as it is written.
#[derive(Clone)]
pub struct OnStderr(Arc<dyn Fn(&str) + Send + Sync>);

impl OnStderr {
    /// Wrap a callback.
    pub fn new(f: impl Fn(&str) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }
}

/// Options of `Session::mcp_call`.
#[derive(Debug, Clone, Default)]
pub struct McpCall {
    /// Tool-execution timeout (the CLI clamps it to 1 s – 10 min; default 2 min).
    pub timeout: Option<Duration>,
    /// The result goes to a program, not a model: content comes back as
    /// the server's own MCP blocks.
    pub result_not_for_model: bool,
    /// Decide the call as if the model made it (permission rules, hooks,
    /// `can_use_tool`), instead of trusting the host.
    pub check_permissions: bool,
}

/// Options of `Session::reload_plugins`.
#[derive(Debug, Clone, Default)]
pub struct ReloadPlugins {
    /// Plugins to load as well, in the `Initialize::plugins` shape.
    pub add_plugins: Vec<serde_json::Value>,
    /// Skip the reload when it would change the tool list the prompt cache
    /// depends on.
    pub hold_on_cache_impact: bool,
    /// Fail with `reload_plugins_busy` instead of reloading during a turn.
    pub refuse_if_busy: bool,
}

macro_rules! opaque_debug {
    ($($t:ident),*) => {$(
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($t), "(..)"))
            }
        }
    )*};
}
opaque_debug!(CanUseTool, OnElicitation, OnUserDialog, OnStderr);

/// How to spawn and configure the CLI process.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// Path of the `claude` executable. Default: `claude` (looked up on `PATH`).
    pub binary: PathBuf,
    /// Working directory of the CLI process. `None` = inherit.
    pub cwd: Option<PathBuf>,
    /// `--model`
    pub model: Option<String>,
    /// `--effort`. `None` = CLI default.
    pub effort: Option<Effort>,
    /// `--permission-mode`
    pub permission_mode: Option<PermissionMode>,
    /// `--fallback-model`: models to try, in order, when the main model is
    /// busy or unavailable (`AgentEvent::ModelFallback` reports a switch).
    pub fallback_model: Option<String>,
    /// `--thinking`.
    pub thinking: Option<Thinking>,
    /// `--thinking-display`.
    pub thinking_display: Option<ThinkingDisplay>,
    /// `--max-thinking-tokens` (older models; newer ones use `thinking`/`effort`).
    pub max_thinking_tokens: Option<u32>,
    /// `--max-turns`: a turn stops with `TurnErrorKind::MaxTurns` after this
    /// many agentic turns.
    pub max_turns: Option<u32>,
    /// `--max-budget-usd`: a turn stops with `TurnErrorKind::MaxBudget` once
    /// the session has spent this much.
    pub max_budget_usd: Option<f64>,
    /// `--task-budget`: API-side task budget in tokens.
    pub task_budget: Option<u32>,
    /// `--resume <id>`: continue an existing session's transcript.
    pub resume: Option<String>,
    /// `--resume-session-at <message id>`: with `resume`, keep the transcript
    /// only up to this message.
    pub resume_session_at: Option<String>,
    /// `--continue`: continue the most recent conversation in `cwd`.
    pub continue_conversation: bool,
    /// `--fork-session`: with `resume`/`continue_conversation`, continue in a
    /// new session id instead of appending to the old transcript.
    pub fork_session: bool,
    /// `--session-id`: use this UUID as the session id.
    pub session_id: Option<String>,
    /// `false` adds `--no-session-persistence`: no transcript is written,
    /// so the session cannot be resumed.
    pub persist_session: bool,
    /// `--name`: display name of the session.
    pub name: Option<String>,
    /// Force Code Mode on: Claude gets the `REPL` tool and works by writing
    /// JavaScript that calls its tools. The stock CLI refuses Code Mode for
    /// SDK sessions, so this runs a patched copy of `binary`, built once and
    /// cached (see `code_mode` module); the installed CLI is not modified.
    pub code_mode: bool,
    /// Enable tool search: sets `ENABLE_TOOL_SEARCH=1`.
    /// Source: chunk-dnmy4w56.js:189
    pub tool_search: bool,
    /// Stream partial chunks (`--include-partial-messages`).
    pub include_partial: bool,
    /// Hook handlers, sent to the CLI in the initialize request.
    pub hooks: HookRegistry,
    /// Tool-permission handler; adds `--permission-prompt-tool stdio`.
    /// Source: `subprocess_cli.py:218`
    pub can_use_tool: Option<CanUseTool>,
    /// In-process tool servers. Their tools are added to `allowed_tools`
    /// automatically: the host defined them, so asking permission is noise.
    pub mcp_servers: Vec<SdkMcpServer>,
    /// `--allowedTools`: tools that run without a permission prompt.
    pub allowed_tools: Vec<String>,
    /// `--disallowedTools`: tools removed from the model's toolset.
    pub disallowed_tools: Vec<String>,
    /// `--tools`: the built-in tools available. `Some(vec![])` = none,
    /// `None` = all.
    pub tools: Option<Vec<String>>,
    /// `--add-dir`: extra directories tools may access.
    pub add_dirs: Vec<PathBuf>,
    /// `--strict-mcp-config`: ignore MCP servers from settings files.
    pub strict_mcp_config: bool,
    /// `--agent`: run the session as this agent.
    pub agent: Option<String>,
    /// `--append-subagent-system-prompt`: appended to every subagent's
    /// system prompt.
    pub append_subagent_system_prompt: Option<String>,
    /// `--plugin-dir`: load plugins from these directories.
    pub plugin_dirs: Vec<PathBuf>,
    /// `--betas`: API beta headers.
    pub betas: Vec<String>,
    /// `--debug-file`: write the CLI's debug log here.
    pub debug_file: Option<PathBuf>,
    /// `--plugin-url`: fetch plugin .zip files for this session only.
    pub plugin_urls: Vec<String>,
    /// `--file`: files to download at startup, as `file_id:relative_path`.
    pub files: Vec<String>,
    /// `--autocompact`: auto-compact window.
    pub autocompact: Option<Autocompact>,
    /// `--permission-prompts`: who answers permission prompts.
    pub permission_prompts: Option<PermissionPrompts>,
    /// `--worktree`: run in a new git worktree; `Some(None)` names it
    /// automatically.
    pub worktree: Option<Option<String>>,
    /// `--bare`: minimal mode (no settings/plugin hooks, LSP, plugin sync, …).
    pub bare: bool,
    /// `--safe-mode`: start with every customization off (CLAUDE.md,
    /// skills, plugins, hooks, MCP servers, …).
    pub safe_mode: bool,
    /// `--restricted`: remove the built-in tools that run commands or code,
    /// and `WebFetch`, unless `tools` names them.
    pub restricted: bool,
    /// `--disable-slash-commands`: no skills or commands.
    pub disable_slash_commands: bool,
    /// `--brief`: give Claude the `SendUserMessage` tool.
    pub brief: bool,
    /// `--include-hook-events`: emit hook lifecycle frames
    /// (`AgentEvent::System`).
    pub include_hook_events: bool,
    /// Extra CLI arguments, appended verbatim — for flags this struct does
    /// not name.
    pub extra_args: Vec<String>,
    /// How long `Session::control` waits for the CLI's answer.
    pub control_timeout: Duration,
    /// Answers MCP elicitations.
    pub on_elicitation: Option<OnElicitation>,
    /// Renders `request_user_dialog` dialogs.
    pub on_user_dialog: Option<OnUserDialog>,
    /// Receives CLI stderr lines live (the last lines are also kept for
    /// `SessionError::ProcessExited`).
    pub on_stderr: Option<OnStderr>,
    /// `--setting-sources`: which settings files to load ("user", "project",
    /// "local"). `Some(vec![])` loads none — the session ignores the user's
    /// global CLAUDE.md, skills, permission rules and hooks, and turns get
    /// much cheaper. `None` = CLI default (all).
    /// Source: `subprocess_cli.py:275–280`
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
            effort: None,
            permission_mode: None,
            fallback_model: None,
            thinking: None,
            thinking_display: None,
            max_thinking_tokens: None,
            max_turns: None,
            max_budget_usd: None,
            task_budget: None,
            resume: None,
            resume_session_at: None,
            continue_conversation: false,
            fork_session: false,
            session_id: None,
            persist_session: true,
            name: None,
            code_mode: false,
            tool_search: false,
            include_partial: false,
            hooks: HookRegistry::new(),
            can_use_tool: None,
            mcp_servers: vec![],
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: None,
            add_dirs: vec![],
            strict_mcp_config: false,
            agent: None,
            append_subagent_system_prompt: None,
            plugin_dirs: vec![],
            betas: vec![],
            debug_file: None,
            plugin_urls: vec![],
            files: vec![],
            autocompact: None,
            permission_prompts: None,
            worktree: None,
            bare: false,
            safe_mode: false,
            restricted: false,
            disable_slash_commands: false,
            brief: false,
            include_hook_events: false,
            extra_args: vec![],
            control_timeout: DEFAULT_CONTROL_TIMEOUT,
            on_elicitation: None,
            on_user_dialog: None,
            on_stderr: None,
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
    /// Unix: process group of the CLI and everything it started (Bash
    /// tools, …). Windows: the CLI's pid, root of its process tree.
    pub(crate) pgid: Option<i32>,
}

/// A live Claude Code session. Cheap to clone — backed by `Arc<SessionInner>`.
///
/// Handlers in `SessionOptions` (`can_use_tool`, hooks, …) are owned by the
/// session; one that needs the session should capture `Session::downgrade()`,
/// since a captured `Session` keeps it, and the CLI, alive forever.
#[derive(Clone)]
pub struct Session(Arc<SessionInner>);

/// A `Session` handle that does not keep the session alive.
#[derive(Clone)]
pub struct WeakSession(std::sync::Weak<SessionInner>);

impl WeakSession {
    /// The session, unless every `Session` handle is gone.
    pub fn upgrade(&self) -> Option<Session> {
        self.0.upgrade().map(Session)
    }
}

impl fmt::Debug for WeakSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WeakSession(..)")
    }
}

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
    cli_version: OnceLock<String>,
    control_timeout: Duration,
    responder: Arc<Responder>,
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
    on_elicitation: Option<OnElicitation>,
    on_user_dialog: Option<OnUserDialog>,
    /// Servers from `SessionOptions::mcp_servers`.
    mcp_servers: Vec<SdkMcpServer>,
    /// Servers set later with `Session::set_mcp_servers`.
    dynamic_mcp_servers: RwLock<Vec<SdkMcpServer>>,
    /// Running handlers of CLI control requests, by request id, so a
    /// `control_cancel_request` can stop them.
    in_flight: DashMap<String, tokio::task::AbortHandle>,
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
        cmd.args(cli_args(&opts));
        let allowed: Vec<String> = opts.allowed_tools.iter().cloned()
            .chain(opts.mcp_servers.iter().flat_map(SdkMcpServer::qualified_tool_names))
            .collect();
        if !allowed.is_empty() {
            cmd.args(["--allowedTools", &allowed.join(",")]);
        }
        if !opts.disallowed_tools.is_empty() {
            cmd.args(["--disallowedTools", &opts.disallowed_tools.join(",")]);
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
        cmd.args(&opts.extra_args);
        if let Some(ref cwd) = opts.cwd {
            cmd.current_dir(cwd);
        }
        // An SDK entrypoint, also when the host itself runs under Claude
        // Desktop or VS Code, whose values the CLI would otherwise inherit
        // and act on. `opts.env` may override it.
        cmd.env("CLAUDE_CODE_ENTRYPOINT", "sdk-cli");
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
        let tail = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let mut drain = tokio::spawn(drain_stderr(stderr, opts.on_stderr.clone(), tail.clone()));
        let exit = Box::pin(async move {
            // Exit is the child's, not stderr's EOF: a tool the CLI started
            // can inherit stderr and hold it open after the CLI is gone.
            let code = child.wait().await.ok().and_then(|s| s.code());
            if tokio::time::timeout(STDERR_LINGER, &mut drain).await.is_err() {
                drain.abort();
            }
            let stderr = Vec::from(std::mem::take(&mut *lock(&tail))).join("\n");
            ExitInfo { code, stderr }
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
            on_elicitation: opts.on_elicitation.clone(),
            on_user_dialog: opts.on_user_dialog.clone(),
            mcp_servers: opts.mcp_servers.clone(),
            dynamic_mcp_servers: RwLock::new(vec![]),
            in_flight: DashMap::new(),
        });
        let reader = tokio::spawn(read_loop(stdout, Reader {
            record,
            fanout: fanout.clone(),
            session_id: session_id.clone(),
            pending: pending.clone(),
            turns_in_flight: turns_in_flight.clone(),
            responder: responder.clone(),
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
            cli_version: OnceLock::new(),
            control_timeout: opts.control_timeout,
            responder,
            turns_in_flight,
            reader_done,
            _tasks: [reader, writer, watchdog],
        }));

        init.hooks = opts.hooks.initialize_config();
        let body = serde_json::to_value(&init)?;
        let body = with_subtype(body, "initialize");
        let response = session.request_with_timeout(&body, INITIALIZE_TIMEOUT).await?;
        let _ = session.0.initialize_response.set(response);
        session.check_cli_version().await?;
        Ok(session)
    }

    /// Ask the CLI its version and refuse one older than `MIN_CLI_VERSION`.
    /// A CLI that cannot answer is let through with a warning.
    async fn check_cli_version(&self) -> Result<()> {
        let reply = self.control(&serde_json::json!({ "subtype": "get_binary_version" })).await;
        let version = match reply {
            Ok(v) => v.get("version").and_then(|v| v.as_str()).map(str::to_owned),
            Err(e) => {
                tracing::warn!(error = %e, "could not read the Claude Code version");
                None
            }
        };
        let Some(version) = version else { return Ok(()) };
        if version_lt(&version, MIN_CLI_VERSION) {
            self.close().await;
            return Err(SessionError::UnsupportedCliVersion { found: version, min: MIN_CLI_VERSION });
        }
        let _ = self.0.cli_version.set(version);
        Ok(())
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
        self.request_with_timeout(body, self.0.control_timeout).await
    }

    /// Interrupt the running turn; its stream ends with an
    /// `AgentEvent::Error` of kind `TurnErrorKind::Execution`. Queued prompts
    /// still run.
    pub async fn interrupt(&self) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "interrupt" })).await.map(|_| ())
    }

    /// Interrupt the running turn and cancel every queued prompt.
    /// Source: binary interrupt schema (`cancel_queued`)
    pub async fn interrupt_all(&self) -> Result<()> {
        let reply = self.control(&serde_json::json!({ "subtype": "interrupt", "cancel_queued": true })).await?;
        // Cancelled prompts get no `result`.
        let cancelled = reply.get("cancelled").and_then(|c| c.as_array()).map_or(0, Vec::len);
        for _ in 0..cancelled {
            decrement(&self.0.turns_in_flight);
        }
        Ok(())
    }

    /// Switch model for the following turns. `None` clears the override, so
    /// the CLI falls back to its default model (not to `SessionOptions::model`).
    /// Source: binary `set_model` schema
    pub async fn set_model(&self, model: Option<&str>) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "set_model", "model": model })).await.map(|_| ())
    }

    /// Switch model and replace the custom system prompt. The new prompt is
    /// sent from the next compaction on (from the next turn with
    /// `Initialize::system_prompt_snapshot: Some(false)`).
    /// Source: binary `set_model` schema (`system_prompt`)
    pub async fn set_model_and_system_prompt(&self, model: &str, system_prompt: &str) -> Result<()> {
        self.control(&serde_json::json!({
            "subtype": "set_model", "model": model, "system_prompt": system_prompt,
        }))
        .await
        .map(|_| ())
    }

    /// Replace the custom system prompt without switching model. `model`
    /// must name the session's current model, or the CLI refuses. CLI
    /// 2.1.329 can confirm the model only while the session runs the
    /// account's default model (no `SessionOptions::model`, no `set_model`);
    /// otherwise use `set_model_and_system_prompt`. The new prompt is sent
    /// from the next compaction on.
    /// Source: binary `set_model` schema (`only_if_model_is_current`)
    pub async fn set_system_prompt(&self, model: &str, system_prompt: &str) -> Result<()> {
        self.control(&serde_json::json!({
            "subtype": "set_model", "model": model, "system_prompt": system_prompt,
            "only_if_model_is_current": true,
        }))
        .await
        .map(|_| ())
    }

    /// Thinking-token budget for the following turns (older models);
    /// `None` restores the session's setting.
    /// Source: binary `set_max_thinking_tokens` schema
    pub async fn set_max_thinking_tokens(&self, tokens: Option<u32>, display: Option<ThinkingDisplay>) -> Result<()> {
        let mut body = serde_json::json!({ "subtype": "set_max_thinking_tokens", "max_thinking_tokens": tokens });
        if let Some(d) = display {
            body["thinking_display"] = d.as_str().into();
        }
        self.control(&body).await.map(|_| ())
    }

    /// Switch effort level for the following turns; `None` clears the
    /// override, so the CLI falls back to its default level (not to
    /// `SessionOptions::effort`).
    /// Source: binary `apply_flag_settings` handler (`effortLevel`)
    pub async fn set_effort(&self, effort: Option<Effort>) -> Result<()> {
        let settings = serde_json::json!({ "effortLevel": effort });
        self.control(&serde_json::json!({ "subtype": "apply_flag_settings", "settings": settings }))
            .await
            .map(|_| ())
    }

    /// Source: binary `set_permission_mode` schema
    pub async fn set_permission_mode(&self, mode: PermissionMode) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "set_permission_mode", "mode": mode })).await.map(|_| ())
    }

    /// Stop a background task by its `task_id` (see `AgentEvent::BackgroundTasks`).
    /// Source: binary `stop_task` schema
    pub async fn stop_task(&self, task_id: &str) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "stop_task", "task_id": task_id })).await.map(|_| ())
    }

    /// Context-window usage of the session, as reported by the CLI.
    /// `exact: true` counts each category with the token-count API; `false`
    /// estimates from the last response's usage, without those calls.
    /// Source: binary `get_context_usage` schema
    pub async fn context_usage(&self, exact: bool) -> Result<serde_json::Value> {
        let detail = if exact { "full" } else { "summary" };
        self.control(&serde_json::json!({ "subtype": "get_context_usage", "detail": detail })).await
    }

    /// Withdraw a prompt: drop it from the queue if it has not started,
    /// else interrupt its turn. Another prompt's turn is never interrupted.
    /// Source: binary `cancel_async_message` and interrupt
    /// (`for_user_message_uuids`) schemas
    pub async fn cancel_prompt(&self, uuid: &str) -> Result<()> {
        let dropped = self
            .control(&serde_json::json!({ "subtype": "cancel_async_message", "message_uuid": uuid }))
            .await?;
        if dropped.get("cancelled").and_then(serde_json::Value::as_bool) == Some(true) {
            // No `result` will come for it.
            decrement(&self.0.turns_in_flight);
            return Ok(());
        }
        self.control(&serde_json::json!({ "subtype": "interrupt", "for_user_message_uuids": [uuid] }))
            .await
            .map(|_| ())
    }

    /// Set the session's display title. Source: binary `rename_session` schema
    pub async fn rename_session(&self, title: &str) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "rename_session", "title": title })).await.map(|_| ())
    }

    /// Restore files Claude changed to their state at `user_message_id`
    /// (the `uuid` of a prompt). `dry_run` reports without changing files.
    /// Source: binary `rewind_files` schema
    pub async fn rewind_files(&self, user_message_id: &str, dry_run: bool) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({
            "subtype": "rewind_files", "user_message_id": user_message_id, "dry_run": dry_run,
        }))
        .await
    }

    /// Connection status of every MCP server. Source: binary `mcp_status` schema
    pub async fn mcp_status(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "mcp_status" })).await
    }

    /// Enable or disable an MCP server. Source: binary `mcp_toggle` schema
    pub async fn mcp_toggle(&self, server: &str, enabled: bool) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "mcp_toggle", "serverName": server, "enabled": enabled }))
            .await
            .map(|_| ())
    }

    /// Reconnect a failed or disconnected MCP server. Source: binary `mcp_reconnect` schema
    pub async fn mcp_reconnect(&self, server: &str) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "mcp_reconnect", "serverName": server })).await.map(|_| ())
    }

    /// Replace the MCP servers added at runtime: in-process `sdk` servers
    /// plus `external` configs (stdio/http/sse, keyed by name). Servers from
    /// `SessionOptions` and settings stay. Their tools are not pre-allowed:
    /// they go through the permission mode and `can_use_tool`.
    /// Source: binary `mcp_set_servers` schema
    pub async fn set_mcp_servers(
        &self,
        sdk: Vec<SdkMcpServer>,
        external: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let servers = server_map(&sdk, Some(&external));
        *self.0.responder.dynamic_mcp_servers.write().unwrap_or_else(std::sync::PoisonError::into_inner) = sdk;
        self.control(&serde_json::json!({ "subtype": "mcp_set_servers", "servers": servers })).await
    }

    /// Models the session can switch to. Source: binary `list_models` schema
    pub async fn list_models(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "list_models" })).await
    }

    /// Effective merged settings plus each source's raw settings.
    /// Source: binary `get_settings` schema
    pub async fn get_settings(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_settings" })).await
    }

    /// Write settings through the CLI's own writer. `local` = the project's
    /// local settings (allowlist: `outputStyle`); otherwise the user's
    /// settings (`effortLevel` only). Source: binary `update_settings` schema
    pub async fn update_settings(&self, local: bool, settings: serde_json::Value) -> Result<serde_json::Value> {
        let source = if local { "localSettings" } else { "userSettings" };
        self.control(&serde_json::json!({ "subtype": "update_settings", "source": source, "settings": settings }))
            .await
    }

    /// Session cost and usage, plus plan rate-limit use when available.
    /// `skip_behaviors` skips the scan of local transcripts (its section is
    /// then null) — enough for a rate-limit meter. Experimental in the CLI.
    /// Source: binary `get_usage` schema
    pub async fn get_usage(&self, skip_behaviors: bool) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_usage", "skip_behaviors": skip_behaviors })).await
    }

    /// Re-read skills from disk. Source: binary `reload_skills` schema
    pub async fn reload_skills(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "reload_skills" })).await
    }

    /// Re-read plugins from disk. Source: binary `reload_plugins` schema
    pub async fn reload_plugins(&self, opts: ReloadPlugins) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({
            "subtype": "reload_plugins",
            "hold_on_cache_impact": opts.hold_on_cache_impact,
            "refuse_if_busy": opts.refuse_if_busy,
        });
        if !opts.add_plugins.is_empty() {
            body["add_plugins"] = serde_json::json!(opts.add_plugins);
        }
        self.control(&body).await
    }

    /// The last output (≤ 8 KiB) of a background shell or Monitor task.
    /// Source: binary `get_task_output` schema
    pub async fn task_output(&self, task_id: &str) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_task_output", "task_id": task_id })).await
    }

    /// Add a working-directory root (a subdirectory of `cwd` or of an
    /// `add_dirs` entry) and reload CLAUDE.md, skills and plugins for it.
    /// Source: binary `register_repo_root` schema
    pub async fn register_repo_root(&self, directory: &str) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({
            "subtype": "register_repo_root", "directory": directory,
            "reload_claude_md": true, "reload_plugins": true, "reload_skills": true,
        }))
        .await
    }

    /// Call a tool of an external MCP server directly, without a model turn
    /// and without a permission check. `tool` is fully qualified
    /// (`mcp__server__tool`). In-process servers are refused: call their
    /// Rust functions directly. Source: binary `mcp_call` schema
    pub async fn mcp_call(&self, tool: &str, arguments: serde_json::Value, opts: McpCall) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({ "subtype": "mcp_call", "tool": tool, "arguments": arguments });
        if let Some(t) = opts.timeout {
            body["timeout_ms"] = serde_json::json!(u64::try_from(t.as_millis()).unwrap_or(u64::MAX));
        }
        if opts.result_not_for_model {
            body["result_not_for_model"] = true.into();
        }
        if opts.check_permissions {
            body["check_permissions"] = true.into();
        }
        self.control(&body).await
    }

    /// Read a `ui://` resource from a connected external MCP server.
    /// Source: binary `mcp_read_resource` schema
    pub async fn mcp_read_resource(&self, server: &str, uri: &str) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "mcp_read_resource", "serverName": server, "uri": uri }))
            .await
    }

    /// Read a file through the CLI, under the session's read permissions
    /// (`path` relative to `cwd`). `base64` for binary files such as images;
    /// otherwise lossy UTF-8. Source: binary `read_file` schema
    pub async fn read_file(&self, path: &str, max_bytes: Option<u64>, base64: bool) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({
            "subtype": "read_file", "path": path,
            "encoding": if base64 { "base64" } else { "utf-8" },
        });
        if let Some(n) = max_bytes {
            body["max_bytes"] = n.into();
        }
        self.control(&body).await
    }

    /// @-mention file suggestions for a partial path, as the terminal shows.
    /// Source: binary `file_suggestions` schema
    pub async fn file_suggestions(&self, query: &str) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "file_suggestions", "query": query })).await
    }

    /// Every hook (settings, session, plugin) grouped by event and matcher.
    /// Source: binary `get_hooks_listing` schema
    pub async fn hooks_listing(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_hooks_listing" })).await
    }

    /// The live permission rules and workspace directories.
    /// Source: binary `list_permission_rules` schema
    pub async fn permission_rules(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "list_permission_rules" })).await
    }

    /// The formatted session cost summary (`/usage` text).
    /// Source: binary `get_session_cost` schema
    pub async fn session_cost(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "get_session_cost" })).await
    }

    /// Re-read output styles from disk. Source: binary `reload_output_styles` schema
    pub async fn reload_output_styles(&self) -> Result<serde_json::Value> {
        self.control(&serde_json::json!({ "subtype": "reload_output_styles" })).await
    }

    /// Tell the CLI the file at `path` was read at `mtime_ms`, so Edit works
    /// on it after the Read left the context. Source: binary `seed_read_state` schema
    pub async fn seed_read_state(&self, path: &str, mtime_ms: u64) -> Result<()> {
        self.control(&serde_json::json!({ "subtype": "seed_read_state", "path": path, "mtime": mtime_ms }))
            .await
            .map(|_| ())
    }

    /// A handle that does not keep the session alive; see `WeakSession`.
    #[must_use]
    pub fn downgrade(&self) -> WeakSession {
        WeakSession(Arc::downgrade(&self.0))
    }

    /// Process id of the CLI (`None` for a replay).
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.0.pgid.and_then(|p| u32::try_from(p).ok())
    }

    /// The CLI's version, read when the session started.
    pub fn cli_version(&self) -> Option<&str> {
        self.0.cli_version.get().map(String::as_str)
    }

    /// Receive every frame the CLI emits from now on. Lossless: a slow
    /// receiver buffers instead of dropping frames, so memory grows until
    /// it catches up. Poll the receiver, or drop it once done.
    #[must_use = "frames buffer until the receiver is dropped"]
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
    #[must_use]
    pub fn initialize_response(&self) -> Option<&serde_json::Value> {
        self.0.initialize_response.get()
    }

    /// Whether the session streams partial messages
    /// (`SessionOptions::include_partial`).
    #[must_use]
    pub fn include_partial(&self) -> bool {
        self.0.include_partial
    }

    /// Whether a prompt sent with `Agent` is still waiting for its `result`.
    #[must_use]
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
    #[must_use]
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
        #[cfg(not(unix))]
        if exit.is_none() {
            kill_tree(self.0.pgid);
            exit = self.wait_exit(CLOSE_GRACE).await;
        }
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
        // Unregisters on every exit, including the caller dropping this future.
        let _registered = PendingGuard { pending: &self.0.pending, request_id: &request_id };
        // The reader marks `ended` before it drops all waiters; a request
        // registered after that would never be answered.
        if lock(&self.0.fanout).ended {
            return Err(self.dead_error().await);
        }
        let envelope = OutboundControlRequest { kind: "control_request", request_id: &request_id, request: body };
        self.send(&envelope).await?;

        let reply = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => reply,
            // Sender dropped: the reader hit EOF because the process exited.
            Ok(Err(_)) => return Err(self.dead_error().await),
            Err(_) => return Err(SessionError::ControlTimeout(subtype)),
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

/// Removes a control request's `pending` entry when dropped.
struct PendingGuard<'a> {
    pending: &'a Pending,
    request_id: &'a str,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        self.pending.remove(self.request_id);
    }
}

impl Drop for SessionInner {
    /// Dropping the last handle ends the CLI and the tools it started:
    /// SIGTERM now, SIGKILL if the CLI is still running after `CLOSE_GRACE`.
    /// Without a Tokio runtime there is no escalation; `close().await` is
    /// the complete shutdown.
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid
            && self.state.load(Ordering::Acquire) != STATE_DEAD
        {
            use nix::sys::signal::{killpg, Signal};
            let group = nix::unistd::Pid::from_raw(pgid);
            let _ = killpg(group, Signal::SIGTERM);
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let mut exit = self.exit.clone();
                runtime.spawn(async move {
                    let exited = tokio::time::timeout(CLOSE_GRACE, exit.wait_for(Option::is_some)).await;
                    if !matches!(exited, Ok(Ok(_))) {
                        let _ = killpg(group, Signal::SIGKILL);
                    }
                });
            }
        }
        #[cfg(not(unix))]
        if self.state.load(Ordering::Acquire) != STATE_DEAD {
            kill_tree(self.pgid);
        }
    }
}

/// Feed every stderr line to `on_stderr` and keep the last ones in `tail`.
/// A panicking callback is logged, not fatal: the drain must keep running.
async fn drain_stderr(
    stderr: tokio::process::ChildStderr,
    on_stderr: Option<OnStderr>,
    tail: Arc<std::sync::Mutex<VecDeque<String>>>,
) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(cb) = &on_stderr
            && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (cb.0)(&line))).is_err()
        {
            tracing::warn!("on_stderr callback panicked");
        }
        let mut tail = lock(&tail);
        if tail.len() == STDERR_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// End the CLI and every process it started (Windows has no process groups).
#[cfg(not(unix))]
fn kill_tree(pid: Option<i32>) {
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Every CLI flag `opts` asks for, besides the fixed stream-json ones.
fn cli_args(opts: &SessionOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![];
    let mut flag = |name: &str, value: Option<String>| {
        if let Some(v) = value {
            args.push(name.into());
            args.push(v);
        }
    };
    flag("--model", opts.model.clone());
    flag("--effort", opts.effort.map(|e| e.as_str().into()));
    flag("--permission-mode", opts.permission_mode.map(|m| m.as_str().into()));
    flag("--fallback-model", opts.fallback_model.clone());
    flag("--thinking", opts.thinking.map(|t| t.as_str().into()));
    flag("--thinking-display", opts.thinking_display.map(|d| d.as_str().into()));
    flag("--max-thinking-tokens", opts.max_thinking_tokens.map(|n| n.to_string()));
    flag("--max-turns", opts.max_turns.map(|n| n.to_string()));
    flag("--max-budget-usd", opts.max_budget_usd.map(|n| n.to_string()));
    flag("--task-budget", opts.task_budget.map(|n| n.to_string()));
    flag("--resume", opts.resume.clone());
    flag("--resume-session-at", opts.resume_session_at.clone());
    flag("--session-id", opts.session_id.clone());
    flag("--name", opts.name.clone());
    flag("--tools", opts.tools.as_ref().map(|t| t.join(",")));
    flag("--agent", opts.agent.clone());
    flag("--append-subagent-system-prompt", opts.append_subagent_system_prompt.clone());
    flag("--betas", (!opts.betas.is_empty()).then(|| opts.betas.join(",")));
    flag("--debug-file", opts.debug_file.as_ref().map(|p| p.display().to_string()));
    flag("--setting-sources", opts.setting_sources.as_ref().map(|s| s.join(",")));
    flag("--permission-prompt-tool", opts.can_use_tool.as_ref().map(|_| "stdio".into()));
    flag(
        "--mcp-config",
        (!opts.mcp_servers.is_empty())
            .then(|| serde_json::json!({ "mcpServers": server_map(&opts.mcp_servers, None) }).to_string()),
    );
    for dir in &opts.add_dirs {
        flag("--add-dir", Some(dir.display().to_string()));
    }
    for dir in &opts.plugin_dirs {
        flag("--plugin-dir", Some(dir.display().to_string()));
    }
    for url in &opts.plugin_urls {
        flag("--plugin-url", Some(url.clone()));
    }
    for spec in &opts.files {
        flag("--file", Some(spec.clone()));
    }
    flag("--autocompact", opts.autocompact.map(super::protocol::Autocompact::to_arg));
    flag("--permission-prompts", opts.permission_prompts.map(|p| p.as_str().into()));
    if let Some(Some(name)) = &opts.worktree {
        flag("--worktree", Some(name.clone()));
    }
    if let Some(None) = opts.worktree {
        args.push("--worktree".into());
    }
    for (on, name) in [
        (opts.bare, "--bare"),
        (opts.safe_mode, "--safe-mode"),
        (opts.restricted, "--restricted"),
        (opts.disable_slash_commands, "--disable-slash-commands"),
        (opts.brief, "--brief"),
        (opts.include_hook_events, "--include-hook-events"),
    ] {
        if on {
            args.push(name.into());
        }
    }
    if opts.continue_conversation {
        args.push("--continue".into());
    }
    if opts.fork_session {
        args.push("--fork-session".into());
    }
    if !opts.persist_session {
        args.push("--no-session-persistence".into());
    }
    if opts.strict_mcp_config {
        args.push("--strict-mcp-config".into());
    }
    args
}

/// `a < b` for dotted versions; a non-numeric suffix ("2.1.329-beta") is ignored.
fn version_lt(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split(['.', '-', ' '])
            .map_while(|part| part.parse().ok())
            .collect()
    };
    parse(a) < parse(b)
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
    let Reader { record, fanout, session_id, pending, turns_in_flight, responder, done } = reader;
    // Runs on EOF and on a panic alike, so streams and waiters never hang.
    let cleanup = ReaderCleanup {
        fanout: fanout.clone(),
        pending,
        turns_in_flight: turns_in_flight.clone(),
        responder: responder.clone(),
        done,
    };
    let mut record = record.map(BufWriter::new);
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(file) = record.as_mut() {
            // Flush once a burst is drained, not per line: a slow disk must
            // not hold up stdout, and the file stays current while idle.
            let flush = lines.get_ref().buffer().is_empty();
            if file.write_all(line.as_bytes()).await.is_err()
                || file.write_all(b"\n").await.is_err()
                || (flush && file.flush().await.is_err())
            {
                tracing::warn!("recording failed; continuing without it");
                record = None;
            }
        }
        match parse_frame(&line) {
            InboundMessage::ControlResponse(r) => {
                if let Some((_, tx)) = cleanup.pending.remove(&r.response.request_id) {
                    let _ = tx.send(r.response);
                }
            }
            InboundMessage::ControlRequest(req) => {
                // Answer without blocking the reader: hooks may run long.
                // The handler runs in its own task, so a panic in user code
                // still sends an error response instead of leaving the CLI
                // waiting. Its abort handle is registered before the next
                // line is read, so a `control_cancel_request` always finds it.
                let request_id = req.request_id.clone();
                let handler = responder.clone();
                let task = tokio::spawn(async move { handler.respond(req).await });
                responder.in_flight.insert(request_id.clone(), task.abort_handle());
                let responder = responder.clone();
                tokio::spawn(async move {
                    let outcome = task.await;
                    responder.in_flight.remove(&request_id);
                    let response = match outcome {
                        Ok(response) => response,
                        // Withdrawn by the CLI, or the CLI is gone: no answer.
                        Err(e) if e.is_cancelled() => return,
                        Err(e) => ControlResponse::error(&request_id, format!("handler failed: {e}")),
                    };
                    responder.write(&response).await;
                });
            }
            InboundMessage::ControlCancelRequest(cancel) => {
                if let Some((_, task)) = responder.in_flight.remove(&cancel.request_id) {
                    task.abort();
                }
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
    if let Some(mut file) = record
        && file.flush().await.is_err()
    {
        tracing::warn!("recording failed to flush");
    }
}

/// End-of-stdout teardown of the reader, run from `Drop`.
struct ReaderCleanup {
    fanout: Arc<std::sync::Mutex<Fanout>>,
    pending: Arc<Pending>,
    turns_in_flight: Arc<AtomicUsize>,
    responder: Arc<Responder>,
    done: watch::Sender<bool>,
}

impl Drop for ReaderCleanup {
    fn drop(&mut self) {
        {
            let mut f = lock(&self.fanout);
            f.ended = true;
            f.subs.clear();
        }
        self.pending.clear();
        self.turns_in_flight.store(0, Ordering::Release);
        // Handlers still waiting (a permission prompt, an elicitation) have
        // no CLI left to answer.
        self.responder.in_flight.retain(|_, task| {
            task.abort();
            false
        });
        let _ = self.done.send(true);
    }
}

/// Decode one stdout line. A `result` the typed model cannot decode still
/// ends its turn, as a minimal result, so no stream waits forever; any other
/// undecodable JSON frame becomes `Other`.
fn parse_frame(line: &str) -> InboundMessage {
    match serde_json::from_str(line) {
        // A frame type this SDK does not model: keep it verbatim.
        Ok(InboundMessage::Unknown) => match serde_json::from_str(line) {
            Ok(value) => InboundMessage::Other(value),
            Err(_) => InboundMessage::Unknown,
        },
        Ok(m) => m,
        Err(e) => {
            let value: Option<serde_json::Value> = serde_json::from_str(line).ok();
            let field = |k: &str| value.as_ref().and_then(|v| v.get(k)).and_then(|v| v.as_str()).map(str::to_owned);
            if field("type").as_deref() == Some("result") {
                tracing::warn!(error = %e, "undecodable result frame; ending the turn with it");
                InboundMessage::Result(ResultMessage {
                    subtype: field("subtype").unwrap_or_else(|| "error_unparsed_result".into()),
                    is_error: value.as_ref().and_then(|v| v.get("is_error")).and_then(serde_json::Value::as_bool).unwrap_or(false),
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
            } else if let Some(value) = value {
                // A known type in a shape this SDK cannot decode: hand it on
                // verbatim rather than losing it.
                tracing::warn!(error = %e, "undecodable frame; passing it on as Other");
                InboundMessage::Other(value)
            } else {
                tracing::debug!(error = %e, line = %line, "non-JSON stdout line");
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
                        ToolDecision::Allow { updated_input, updated_permissions } => ToolDecision::Allow {
                            updated_input: Some(updated_input.unwrap_or(original)),
                            updated_permissions,
                        },
                        deny @ ToolDecision::Deny { .. } => deny,
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
                let server = self.mcp_servers.iter().find(|s| s.name == m.server_name).cloned().or_else(|| {
                    let dynamic = self.dynamic_mcp_servers.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                    dynamic.iter().find(|s| s.name == m.server_name).cloned()
                });
                let reply = match server {
                    Some(server) => server.handle(&m.message).instrument(span).await,
                    None => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": m.message.get("id"),
                        "error": { "code": -32601, "message": format!("Server '{}' not found", m.server_name) },
                    }),
                };
                ControlResponse::success(&req.request_id, serde_json::json!({ "mcp_response": reply }))
            }
            ControlRequestBody::Elicitation(e) => match &self.on_elicitation {
                Some(handler) => match serde_json::to_value((handler.0)(e).await) {
                    Ok(v) => ControlResponse::success(&req.request_id, v),
                    Err(e) => ControlResponse::error(&req.request_id, e.to_string()),
                },
                None => ControlResponse::error(&req.request_id, "no on_elicitation handler"),
            },
            ControlRequestBody::RequestUserDialog(d) => match &self.on_user_dialog {
                Some(handler) => match serde_json::to_value((handler.0)(d).await) {
                    Ok(v) => ControlResponse::success(&req.request_id, v),
                    Err(e) => ControlResponse::error(&req.request_id, e.to_string()),
                },
                None => ControlResponse::error(&req.request_id, "no on_user_dialog handler"),
            },
            ControlRequestBody::Unsupported => {
                ControlResponse::error(&req.request_id, "unsupported control request subtype")
            }
        }
    }

    async fn write(&self, response: &ControlResponse) {
        match serde_json::to_vec(response) {
            Ok(bytes) => if let Some(tx) = self.stdin_tx.upgrade() {
                let _ = tx.send(bytes).await;
            } else { tracing::debug!("session closed; dropping control response") },
            Err(e) => tracing::warn!(error = %e, "failed to serialize control response"),
        }
    }
}
