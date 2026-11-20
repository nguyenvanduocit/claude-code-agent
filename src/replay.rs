//! Deterministic record/replay — run the SDK without a CLI process or API cost.
//!
//! Record: `SessionOptions::record = Some(path)` writes every stdout line of a
//! live session to a JSONL file.
//! Replay: `replay::replay(path, opts)` returns a `Session` backed by an
//! in-memory fake CLI that answers `initialize` with the recorded response and
//! plays back one recorded turn per user message.
//!
//! Control requests the CLI made during recording (hooks, `can_use_tool`,
//! `mcp_message`) are not replayed: their answers came from host code, which a
//! replay does not re-run.
use std::path::Path;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::Initialize;
use crate::session::{Result, Session, SessionError, SessionOptions, Transport};

/// A recording split into the initialize response and per-turn frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    /// Payload of the CLI's answer to `initialize`.
    pub initialize_response: Value,
    /// Frames of each prompted turn, ending with its `result` line. Turns the
    /// CLI started itself are kept in front of the following prompted turn.
    pub turns: Vec<Vec<String>>,
}

impl Recording {
    /// Parse a JSONL recording.
    pub fn parse(jsonl: &str) -> Result<Self> {
        let mut initialize_response = None;
        let mut turns = vec![];
        let mut current = vec![];
        for (n, line) in jsonl.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let frame: Value = serde_json::from_str(line)
                .map_err(|e| SessionError::InvalidRecording(format!("line {}: {e}", n + 1)))?;
            match frame.get("type").and_then(Value::as_str) {
                Some("control_response") => {
                    if initialize_response.is_none() {
                        initialize_response =
                            Some(frame["response"].get("response").cloned().unwrap_or(Value::Null));
                    }
                }
                Some("control_request") => {}
                // A result with `origin` ends a turn the CLI started itself;
                // it is replayed in front of the next prompted turn. Same
                // rule as the live session: `origin: null` is no origin.
                Some("result") if frame.get("origin").is_none_or(Value::is_null) => {
                    current.push(line.to_string());
                    turns.push(std::mem::take(&mut current));
                }
                _ => current.push(line.to_string()),
            }
        }
        // Frames after the last prompted result (a turn the CLI started
        // itself, late system frames) play after that result.
        if let Some(last) = turns.last_mut() {
            last.append(&mut current);
        }
        let initialize_response = initialize_response
            .ok_or_else(|| SessionError::InvalidRecording("no initialize response".into()))?;
        Ok(Self { initialize_response, turns })
    }

    /// Read and parse a JSONL recording file.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::parse(&tokio::fs::read_to_string(path).await?)
    }
}

/// A Session that replays `path` instead of running the CLI.
/// Use the same `include_partial` as the recording.
pub async fn replay(path: impl AsRef<Path>, opts: SessionOptions) -> Result<Session> {
    let recording = Recording::load(path).await?;
    let record = crate::session::open_record(&opts).await?;
    let (host, cli) = tokio::io::duplex(1 << 16);
    let (host_out, host_in) = tokio::io::split(host);
    let fake = tokio::spawn(fake_cli(cli, recording));
    let exit = Box::pin(async move { fake.await.ok().map(|()| 0) });
    let stderr_tail = Box::pin(async { String::new() });
    let transport =
        Transport {
            stdin: Box::new(host_in),
            stdout: Box::new(host_out),
            exit,
            stderr_tail,
            pgid: None,
            #[cfg(windows)]
            job: None,
        };
    Session::connect(transport, opts, Initialize::default(), record).await
}

async fn fake_cli(io: tokio::io::DuplexStream, recording: Recording) {
    let (read, mut write) = tokio::io::split(io);
    let mut lines = BufReader::new(read).lines();
    let mut turns = recording.turns.into_iter();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let out: Vec<String> = match msg.get("type").and_then(Value::as_str) {
            Some("control_request") => {
                let is_init = msg["request"]["subtype"] == "initialize";
                let payload = if is_init { recording.initialize_response.clone() } else { json!({}) };
                vec![json!({
                    "type": "control_response",
                    "response": { "subtype": "success", "request_id": msg["request_id"], "response": payload },
                })
                .to_string()]
            }
            Some("user") => turns.next().map_or_else(|| {
                vec![
                    json!({ "type": "user", "isReplay": true, "uuid": msg["uuid"], "message": msg["message"] })
                        .to_string(),
                    json!({
                        "type": "result",
                        "subtype": "error_during_execution",
                        "is_error": true,
                        "errors": ["replay exhausted: no recorded turn left"],
                        "user_message_uuids": [msg["uuid"]],
                    })
                    .to_string(),
                ]
            }, |frames| for_prompt(frames, &msg["uuid"])),
            _ => vec![],
        };
        for frame in out {
            if write.write_all(frame.as_bytes()).await.is_err() || write.write_all(b"\n").await.is_err() {
                return;
            }
        }
    }
}

/// Point the recorded turn at the prompt being replayed: its echo, and the
/// prompts its `result` answered. A prompt the CLI refused before running
/// it has no echo; only the `result` names it.
fn for_prompt(frames: Vec<String>, uuid: &Value) -> Vec<String> {
    frames
        .into_iter()
        .map(|line| match serde_json::from_str::<Value>(&line) {
            Ok(mut f) if f["type"] == "user" && f["isReplay"] == true => {
                f["uuid"] = uuid.clone();
                f.to_string()
            }
            Ok(mut f) if f["type"] == "result" && f.get("origin").is_none_or(Value::is_null) => {
                f["user_message_uuids"] = json!([uuid]);
                f.to_string()
            }
            _ => line,
        })
        .collect()
}
