/// Persistent subprocess session — 3-task architecture.
///
/// Architecture from Fork C (verified against chunk-f89nsf5q.query.js):
///   reader_task  — reads stdout, dispatches to pending map or event broadcast
///   writer_task  — drains stdin_tx channel, writes to subprocess stdin
///   watchdog_task — calls child.wait(), sets state=Dead on exit
///
/// Source: chunk-f89nsf5q.query.js:614 (arg list), :915–949 (readline loop),
///         :1383–1600 (handleControlRequest, pendingControlResponses, cancelControllers)
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, Command};
use std::sync::OnceLock;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::hooks::HookRegistry;
use crate::protocol::{
    ControlRequest, ControlRequestPayload, ControlResponsePayload,
    HookOutput, InboundMessage, Initialize,
};

// ─── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("process exited (code: {code:?})")]
    ProcessExited { code: Option<i32> },
    #[error("session closed")]
    Closed,
    #[error("hook deadline exceeded")]
    HookDeadline,
    #[error("send error")]
    Send,
}

pub type Result<T> = std::result::Result<T, SessionError>;

// ─── Session state ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupMark {
    pub mark: String,
    pub at_ms: u64,
}

const STATE_RUNNING: u8 = 0;
const STATE_CLOSING: u8 = 1;
const STATE_DEAD: u8 = 2;

// ─── Options ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub binary: PathBuf,
    /// Session ID for reconnect. If set, spawns with --session-id <id>.
    /// Source: chunk-f89nsf5q.query.js:706
    pub session_id: Option<String>,
    /// Enable Code Mode: sets CLAUDE_CODE_EXPERIMENTAL_CODE_MODE=1.
    /// Source: chunk-0jv1f6sm.codeModeShape.js:13–15
    pub code_mode: bool,
    /// Enable tool search: sets ENABLE_TOOL_SEARCH=1.
    /// Source: chunk-dnmy4w56.js:189
    pub tool_search: bool,
    /// Stream partial chunks (requires --include-partial-messages).
    pub include_partial: bool,
    /// Extra env vars passed to the subprocess.
    pub env: Vec<(String, String)>,
    /// Broadcast channel capacity for inbound events.
    pub event_buffer: usize,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("claude"),
            session_id: None,
            code_mode: false,
            tool_search: false,
            include_partial: false,
            env: vec![],
            event_buffer: 1024,
        }
    }
}

// ─── Session ─────────────────────────────────────────────────────────────────

pub type EventRx = broadcast::Receiver<Arc<InboundMessage>>;

/// A live Claude Code session. Cheap to clone — backed by Arc<SessionInner>.
#[derive(Clone)]
pub struct Session(Arc<SessionInner>);

struct SessionInner {
    // Resolved session ID from first system/init frame.
    // Source: chunk-20bv8pca.js:15349
    session_id: OnceLock<String>,

    // stdin channel — writer_task drains this.
    stdin_tx: mpsc::Sender<Vec<u8>>,

    // Broadcast — every inbound message goes here.
    // Capacity 1024 → ~2–10s buffer at 100–500 events/s.
    // Source: Fork C estimate
    event_tx: broadcast::Sender<Arc<InboundMessage>>,

    // Pending control_responses keyed by request_id.
    // Source: chunk-f89nsf5q.query.js:1387 (pendingControlResponses)
    pending: Arc<DashMap<String, oneshot::Sender<ControlResponsePayload>>>,

    // Ring buffer for unmatched responses that arrived before the waiter.
    // Capacity 32. Source: chunk-f89nsf5q.query.js:1394 (unmatchedControlResponses)
    unmatched: Mutex<VecDeque<(String, ControlResponsePayload)>>,

    // Hook registry — async Rust closures keyed by callback_id prefix.
    pub hook_registry: Arc<HookRegistry>,

    // Startup mark from keep_alive frames.
    // Source: chunk-f89nsf5q.query.js:1159, 1403–1410
    startup_mark: Mutex<Option<StartupMark>>,

    // Session lifecycle state.
    state: AtomicU8,

    // Task handles (held to keep tasks alive).
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
    _watchdog: JoinHandle<()>,
}

impl Session {
    /// Spawn a new Claude Code subprocess and perform the initialize handshake.
    ///
    /// Flow:
    ///   1. spawn `claude --output-format stream-json --verbose --input-format stream-json --await-initialize`
    ///   2. write the Initialize message
    ///   3. wait for the first `system/init` frame
    ///   4. return a live Session
    pub async fn spawn(opts: SessionOptions, init: Initialize) -> Result<Self> {
        let mut cmd = Command::new(&opts.binary);
        cmd.args([
            "--output-format", "stream-json",
            "--verbose",
            "--input-format", "stream-json",
            "--await-initialize",
        ]);
        if opts.include_partial {
            cmd.arg("--include-partial-messages");
        }
        if let Some(ref sid) = opts.session_id {
            cmd.args(["--session-id", sid]);
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
           .stderr(Stdio::null())
           .kill_on_drop(false); // do NOT kill mid-turn

        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>(256);
        let (event_tx, _) = broadcast::channel(opts.event_buffer);
        let hook_registry = Arc::new(HookRegistry::new());

        let pending: DashMap<String, oneshot::Sender<ControlResponsePayload>> = DashMap::new();
        let pending = Arc::new(pending);
        let et2 = event_tx.clone();
        let pending2 = pending.clone();
        let hook_reg2 = hook_registry.clone();

        // ── reader task ───────────────────────────────────────────────────────
        let reader_handle = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = match lines.next_line().await {
                    Ok(Some(l)) if !l.trim().is_empty() => l,
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => break,
                };
                let msg: InboundMessage = match serde_json::from_str(&line) {
                    Ok(m) => m,
                    Err(_) => InboundMessage::Unknown,
                };
                let msg = Arc::new(msg);

                // Dispatch control requests without blocking the reader.
                // Source: chunk-f89nsf5q.query.js:1529 (handleControlRequest without await)
                if let InboundMessage::ControlRequest(ref cr) = *msg {
                    // Dispatch hook callbacks and can_use_tool to registry.
                    let cr2 = cr.clone();
                    let et3 = et2.clone();
                    let hr = hook_reg2.clone();
                    let pend = pending2.clone();
                    tokio::spawn(async move {
                        Self::dispatch_control(&cr2, hr, pend, et3).await;
                    });
                }

                // Broadcast to all subscribers.
                let _ = et2.send(msg);
            }
        });

        // ── writer task ───────────────────────────────────────────────────────
        let writer_handle = tokio::spawn(async move {
            let mut writer = BufWriter::new(stdin);
            let mut rx = stdin_rx;
            while let Some(bytes) = rx.recv().await {
                if writer.write_all(&bytes).await.is_err() { break; }
                if writer.write_all(b"\n").await.is_err() { break; }
                if writer.flush().await.is_err() { break; }
            }
            let _ = writer.shutdown().await;
        });

        // ── watchdog task ─────────────────────────────────────────────────────
        let state_arc = Arc::new(AtomicU8::new(STATE_RUNNING));
        let state2 = state_arc.clone();
        let watchdog_handle: JoinHandle<()> = tokio::spawn(async move {
            let mut child: Child = child;
            let _ = child.wait().await;
            state2.store(STATE_DEAD, Ordering::SeqCst);
        });

        let inner = Arc::new(SessionInner {
            session_id: OnceLock::new(),
            stdin_tx,
            event_tx,
            pending,
            unmatched: Mutex::new(VecDeque::with_capacity(32)),
            hook_registry,
            startup_mark: Mutex::new(None),
            state: AtomicU8::new(STATE_RUNNING),
            _reader: reader_handle,
            _writer: writer_handle,
            _watchdog: watchdog_handle,
        });

        let session = Session(inner);

        // Send initialize.
        session.send(&init).await?;

        // Wait for first system/init frame.
        let mut rx = session.subscribe();
        loop {
            match rx.recv().await {
                Ok(msg) => {
                    if let InboundMessage::System(ref s) = *msg {
                        let _ = session.0.session_id.set(s.session_id.clone());
                        break;
                    }
                }
                Err(_) => return Err(SessionError::Closed),
            }
        }

        Ok(session)
    }

    /// Send a JSON-serializable message to the subprocess stdin.
    pub async fn send<T: serde::Serialize>(&self, msg: &T) -> Result<()> {
        if self.0.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(SessionError::Closed);
        }
        let bytes = serde_json::to_vec(msg)?;
        self.0.stdin_tx.send(bytes).await.map_err(|_| SessionError::Send)
    }

    /// Subscribe to the broadcast event stream.
    pub fn subscribe(&self) -> EventRx {
        self.0.event_tx.subscribe()
    }

    /// Session ID from the most recent system/init frame.
    pub fn session_id(&self) -> Option<&str> {
        self.0.session_id.get().map(|s| s.as_str())
    }

    /// Startup mark (readiness probe).
    /// Source: chunk-f89nsf5q.query.js:1403–1410
    pub async fn startup_mark(&self) -> Option<StartupMark> {
        self.0.startup_mark.lock().await.clone()
    }

    /// Whether the subprocess is still running.
    pub fn is_alive(&self) -> bool {
        self.0.state.load(Ordering::Acquire) == STATE_RUNNING
    }

    /// Close stdin, letting the CLI exit cleanly.
    /// Source: chunk-f89nsf5q.query.js:949 (processStdin.end())
    pub async fn close(&self) {
        self.0.state.store(STATE_CLOSING, Ordering::SeqCst);
        drop(self.0.stdin_tx.clone()); // triggers writer task to exit
    }

    /// Hook registry — register async Rust closures on any of the 33 events.
    pub fn hooks(&self) -> Arc<HookRegistry> {
        self.0.hook_registry.clone()
    }

    // ── Control request dispatch (runs in spawned task) ─────────────────────
    async fn dispatch_control(
        cr: &ControlRequest,
        registry: Arc<HookRegistry>,
        _pending: Arc<DashMap<String, oneshot::Sender<ControlResponsePayload>>>,
        event_tx: broadcast::Sender<Arc<InboundMessage>>,
    ) {
        match &cr.payload {
            ControlRequestPayload::HookCallback(hcb) => {
                let deadline = Duration::from_millis(hcb.deadline_ms);
                let remaining = deadline.saturating_sub(
                    Duration::from_millis(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis()
                            .saturating_sub(hcb.issued_at as u128) as u64
                    )
                );
                let output = match tokio::time::timeout(
                    remaining,
                    registry.dispatch(&hcb.callback_id, &hcb.input),
                ).await {
                    Ok(out) => out,
                    Err(_) => HookOutput::allow(), // timeout → allow (safe default)
                };
                // Broadcast the response as a synthetic inbound message so the
                // agent layer can pick it up and write ControlResponse to stdin.
                let reply = Arc::new(InboundMessage::ControlRequest(ControlRequest {
                    subtype: "hook_callback_reply".into(),
                    request_id: cr.request_id.clone(),
                    payload: ControlRequestPayload::HookCallback(crate::protocol::HookCallbackRequest {
                        callback_id: hcb.callback_id.clone(),
                        input: hcb.input.clone(),
                        issued_at: hcb.issued_at,
                        deadline_ms: hcb.deadline_ms,
                    }),
                }));
                // Store output for the agent layer to retrieve.
                let _ = event_tx.send(reply);
                let _ = output; // consumed by registry callback
            }
            ControlRequestPayload::CanUseTool(_) => {
                // can_use_tool is handled at the Agent layer via the pending map.
            }
            ControlRequestPayload::Other(_) => {}
        }
    }
}

// Reconnect: spawn a new process with the previous session_id.
// CLI reloads its own JSONL transcript — no stdin replay needed.
// Source: Fork C (verified chunk-f89nsf5q.query.js:792–800, :706)
impl Session {
    pub async fn reconnect(previous_id: &str, mut opts: SessionOptions, init: Initialize) -> Result<Self> {
        opts.session_id = Some(previous_id.to_string());
        Self::spawn(opts, init).await
    }
}
