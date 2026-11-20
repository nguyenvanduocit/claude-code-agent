//! Session behaviour against a fake CLI, on every platform. Needs no real
//! CLI: this executable doubles as the fake one (`ROLE` picks what it does).
//!
//! - `close()` ends the CLI, a child in its group, and (Unix) a child in a
//!   process group of its own, as the real CLI starts hooks and Bash tools,
//!   both when the CLI outlives stdin EOF and when it exits on it. A line
//!   that is not UTF-8 does not stop the reader; requests after `close()`
//!   fail with `Closed`.
//! - One `result` answering two merged prompts ends both turns.
//! - Two sessions sharing an `SdkMcpServer` cancel only their own calls,
//!   and tool handlers see their session's id.
//! - A prompt dropped before it is written does not leave `is_busy()` set,
//!   and a control request times out even when stdin is full.
//! - Control requests fail fast once the CLI exits, even while a process it
//!   started holds stdout open.
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use claude_code_agent::protocol::ToolDef;
use claude_code_agent::{
    Agent, Initialize, SdkMcpServer, SdkTool, Session, SessionError, SessionOptions, ToolContext, ToolResult,
};

const ROLE: &str = "CLAUDE_SDK_TEST_ROLE";
const PID_FILE: &str = "CLAUDE_SDK_TEST_PID_FILE";
const WHO: &str = "CLAUDE_SDK_TEST_WHO";

fn main() {
    match std::env::var(ROLE).as_deref() {
        Ok("cli") => tree_cli(true),
        Ok("eof_cli") => tree_cli(false),
        Ok("merged_cli") => merged_cli(),
        Ok("mcp_cli") => mcp_cli(),
        Ok("hung_cli") => {
            handshake();
            std::thread::sleep(Duration::from_secs(600));
        }
        Ok("held_cli") => held_cli(),
        Ok("sleeper") => std::thread::sleep(Duration::from_secs(600)),
        _ => {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(close_ends_process_tree("cli"));
            rt.block_on(close_ends_process_tree("eof_cli"));
            rt.block_on(merged_prompts_end_together());
            rt.block_on(mcp_cancel_stays_in_its_session());
            rt.block_on(hung_cli_does_not_wedge_the_session());
            rt.block_on(exit_fails_requests_while_stdout_is_held());
        }
    }
}

// ── fake CLI roles ───────────────────────────────────────────────────────────

fn emit(line: &serde_json::Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}").unwrap();
    out.flush().unwrap();
}

/// Answer `line` if it is a control request.
fn answer(line: &str) {
    let msg: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
    if msg["type"] != "control_request" {
        return;
    }
    let payload = match msg["request"]["subtype"].as_str() {
        Some("get_binary_version") => serde_json::json!({ "version": claude_code_agent::session::MIN_CLI_VERSION }),
        _ => serde_json::json!({}),
    };
    emit(&serde_json::json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": msg["request_id"], "response": payload },
    }));
}

/// Answer `initialize` and `get_binary_version`.
fn handshake() {
    let mut lines = std::io::stdin().lock().lines();
    for _ in 0..2 {
        answer(&lines.next().unwrap().unwrap());
    }
}

fn sleeper() -> std::process::Command {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.env(ROLE, "sleeper");
    cmd
}

/// Start a child in the CLI's group and (Unix) one in a group of its own,
/// write their pids, write a line that is not UTF-8, answer control
/// requests, then on stdin EOF wait for the children (`outlive_eof`) or exit.
fn tree_cli(outlive_eof: bool) {
    let mut children = vec![sleeper().spawn().unwrap()];
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        children.push(sleeper().process_group(0).spawn().unwrap());
    }
    let pids: Vec<String> = children.iter().map(|c| c.id().to_string()).collect();
    std::fs::write(std::env::var(PID_FILE).unwrap(), pids.join(" ")).unwrap();
    for out in [&mut std::io::stdout() as &mut dyn Write, &mut std::io::stderr()] {
        out.write_all(b"\xff\xfe not utf-8\n").unwrap();
        out.flush().unwrap();
    }
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        answer(&line);
    }
    if outlive_eof {
        for child in &mut children {
            let _ = child.wait();
        }
    }
}

/// Take two prompts, then end both with one `result`, as the CLI does when
/// it merges prompts sent close together into one turn.
fn merged_cli() {
    handshake();
    let mut uuids = vec![];
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let msg: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if msg["type"] == "user" {
            uuids.push(msg["uuid"].clone());
        }
        if uuids.len() == 2 {
            break;
        }
    }
    for uuid in &uuids {
        emit(&serde_json::json!({ "type": "user", "isReplay": true, "uuid": uuid,
            "message": { "role": "user", "content": "hi" } }));
    }
    emit(&serde_json::json!({ "type": "result", "subtype": "success", "is_error": false,
        "user_message_uuids": uuids }));
    for _ in std::io::stdin().lock().lines().map_while(Result::ok) {}
}

/// Session `sess-<WHO>` calls tool `slow` with JSON-RPC id 0 (as every CLI's
/// first call is); session A cancels its request 0 right after.
fn mcp_cli() {
    handshake();
    let who = std::env::var(WHO).unwrap();
    emit(&serde_json::json!({ "type": "system", "subtype": "init", "session_id": format!("sess-{who}") }));
    let mcp = |id: &str, message: serde_json::Value| {
        serde_json::json!({ "type": "control_request", "request_id": id,
            "request": { "subtype": "mcp_message", "server_name": "srv", "message": message } })
    };
    emit(&mcp("m1", serde_json::json!({ "jsonrpc": "2.0", "id": 0, "method": "tools/call",
        "params": { "name": "slow", "arguments": { "who": who } } })));
    if who == "A" {
        emit(&mcp("m2", serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": { "requestId": 0 } })));
    }
    for _ in std::io::stdin().lock().lines().map_while(Result::ok) {}
}

/// Start a child that inherits stdout, leave the next request unanswered
/// for a moment, then exit.
#[expect(clippy::zombie_processes, reason = "the child must outlive this fake CLI to hold stdout open")]
fn held_cli() {
    handshake();
    let child = sleeper().spawn().unwrap();
    std::fs::write(std::env::var(PID_FILE).unwrap(), child.id().to_string()).unwrap();
    std::thread::sleep(Duration::from_millis(1000));
}

// ── checks ───────────────────────────────────────────────────────────────────

fn opts(role: &str, extra: &[(&str, String)]) -> SessionOptions {
    let mut env = vec![(ROLE.to_string(), role.to_string())];
    env.extend(extra.iter().map(|(k, v)| ((*k).to_string(), v.clone())));
    SessionOptions { binary: std::env::current_exe().unwrap(), env, ..Default::default() }
}

fn pid_file(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("claude-code-agent-{name}-{}.pid", std::process::id()))
}

async fn close_ends_process_tree(role: &str) {
    let file = pid_file(role);
    let session = Session::spawn(opts(role, &[(PID_FILE, file.display().to_string())]), Initialize::default())
        .await
        .expect("spawn fake cli");
    let cli_pid = session.pid().expect("cli pid");
    let children: Vec<u32> = wait_for(|| {
        let pids: Vec<u32> = std::fs::read_to_string(&file).ok()?.split_whitespace().filter_map(|p| p.parse().ok()).collect();
        (!pids.is_empty()).then_some(pids)
    });
    let mut all = children.clone();
    all.push(cli_pid);
    assert!(all.iter().all(|&p| alive(p)), "processes did not start");
    session.interrupt().await.expect("the reader outlives a line that is not UTF-8");
    session.close().await;
    let _ = std::fs::remove_file(&file);
    wait_for(|| all.iter().all(|&p| !alive(p)).then_some(()));
    let after = session.interrupt().await;
    assert!(matches!(after, Err(SessionError::Closed)), "got {after:?}");
    println!("fake_cli ({role}): close() ended the CLI ({cli_pid}) and its children {children:?}");
}

async fn merged_prompts_end_together() {
    let session = Session::spawn(opts("merged_cli", &[]), Initialize::default()).await.expect("spawn merged cli");
    let agent = Agent::new(session.clone());
    let streams = [agent.prompt("a").await.expect("send a"), agent.prompt("b").await.expect("send b")];
    for mut stream in streams {
        let end = tokio::time::timeout(Duration::from_secs(5), async {
            use tokio_stream::StreamExt;
            while let Some(ev) = stream.next().await {
                if matches!(ev, claude_code_agent::AgentEvent::Done(_) | claude_code_agent::AgentEvent::Error(_)) {
                    return Some(ev);
                }
            }
            None
        })
        .await
        .expect("merged turn must end");
        assert!(matches!(end, Some(claude_code_agent::AgentEvent::Done(_))), "got {end:?}");
    }
    assert!(!session.is_busy(), "one result answering two prompts must end both");
    session.close().await;
    println!("fake_cli: one result for two merged prompts ends both and leaves is_busy() false");
}

async fn mcp_cancel_stays_in_its_session() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let tool_log = log.clone();
    let tool = SdkTool::with_context(ToolDef { name: "slow".into(), ..Default::default() }, move |args, ctx: ToolContext| {
        let log = tool_log.clone();
        let who = args["who"].as_str().unwrap_or("?").to_string();
        log.lock().unwrap().push(format!("{who} in {}", ctx.session_id.unwrap_or_default()));
        let finished = Finished(log, who, false);
        async move {
            let mut finished = finished;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            finished.2 = true;
            ToolResult::text("done")
        }
    });
    let server = SdkMcpServer::new("srv").tool(tool);
    let spawn = |who: &str| {
        let mut o = opts("mcp_cli", &[(WHO, who.to_string())]);
        o.mcp_servers = vec![server.clone()];
        Session::spawn(o, Initialize::default())
    };
    let (a, b) = tokio::join!(spawn("A"), spawn("B"));
    let (a, b) = (a.expect("spawn A"), b.expect("spawn B"));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let mut seen = log.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, ["A cancelled", "A in sess-A", "B done", "B in sess-B"]);
    a.close().await;
    b.close().await;
    println!("fake_cli: MCP cancel stayed in its session; tools saw their session ids");
}

/// Logs `<who> done` or, when dropped unfinished (cancelled), `<who> cancelled`.
struct Finished(Arc<Mutex<Vec<String>>>, String, bool);

impl Drop for Finished {
    fn drop(&mut self) {
        let outcome = if self.2 { "done" } else { "cancelled" };
        self.0.lock().unwrap().push(format!("{} {outcome}", self.1));
    }
}

async fn hung_cli_does_not_wedge_the_session() {
    let mut o = opts("hung_cli", &[]);
    o.control_timeout = Duration::from_secs(1);
    let session = Session::spawn(o, Initialize::default()).await.expect("spawn hung cli");
    // The CLI reads nothing more: fill the pipe and the stdin queue.
    let noise = serde_json::json!({ "type": "noise", "pad": "x".repeat(100_000) });
    while tokio::time::timeout(Duration::from_millis(200), session.send(&noise)).await.is_ok() {}

    let agent = Agent::new(session.clone());
    let dropped = tokio::time::timeout(Duration::from_millis(200), agent.prompt("hi")).await;
    assert!(dropped.is_err(), "prompt should block on the full stdin");
    assert!(!session.is_busy(), "a prompt dropped before its write must not count as in flight");

    let started = Instant::now();
    let interrupted = tokio::time::timeout(Duration::from_secs(10), session.interrupt()).await;
    assert!(
        matches!(interrupted, Ok(Err(SessionError::ControlTimeout(_)))),
        "interrupt() must time out on a full stdin, got {interrupted:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    session.close().await;
    println!("fake_cli: a hung CLI times out control requests and keeps is_busy() right");
}

async fn exit_fails_requests_while_stdout_is_held() {
    let file = pid_file("held");
    let mut o = opts("held_cli", &[(PID_FILE, file.display().to_string())]);
    o.control_timeout = Duration::from_secs(20);
    let session = Session::spawn(o, Initialize::default()).await.expect("spawn held cli");
    // Sent while the CLI runs; it exits without answering.
    let started = Instant::now();
    let reply = session.interrupt().await;
    let waited = started.elapsed();
    let holder: u32 = wait_for(|| std::fs::read_to_string(&file).ok()?.trim().parse().ok());
    kill(holder);
    let _ = std::fs::remove_file(&file);
    assert!(matches!(reply, Err(SessionError::ProcessExited { .. })), "got {reply:?}");
    assert!(waited < Duration::from_secs(4), "waited {waited:?} for a request to a dead CLI");
    println!("fake_cli: requests fail fast after exit while stdout is held open");
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn wait_for<T>(mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // A zombie of another parent counts as gone once `ps` stops listing it
    // as running.
    let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

#[cfg(unix)]
fn kill(pid: u32) {
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
}

#[cfg(windows)]
fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status();
}
