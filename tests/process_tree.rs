//! `Session::close` ends the CLI and the processes it started, on every
//! platform (process group on Unix, `taskkill /T` on Windows). Needs no real
//! CLI: this executable doubles as a fake one that keeps running past stdin
//! EOF (as the real CLI does while a background command runs) and starts a
//! long-lived child.
use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use claude_code_agent::{Initialize, Session, SessionOptions};

const ROLE: &str = "CLAUDE_SDK_TEST_ROLE";
const CHILD_PID_FILE: &str = "CLAUDE_SDK_TEST_CHILD_PID_FILE";

fn main() {
    match std::env::var(ROLE).as_deref() {
        Ok("cli") => fake_cli(),
        Ok("sleeper") => std::thread::sleep(Duration::from_secs(600)),
        _ => close_ends_process_tree(),
    }
}

/// Answer control requests on stdin, then outlive stdin EOF.
fn fake_cli() {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .env(ROLE, "sleeper")
        .spawn()
        .unwrap();
    std::fs::write(std::env::var(CHILD_PID_FILE).unwrap(), child.id().to_string()).unwrap();
    let stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let msg: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if msg["type"] != "control_request" {
            continue;
        }
        let payload = match msg["request"]["subtype"].as_str() {
            Some("get_binary_version") => serde_json::json!({ "version": claude_code_agent::session::MIN_CLI_VERSION }),
            _ => serde_json::json!({}),
        };
        let reply = serde_json::json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": msg["request_id"], "response": payload },
        });
        let mut out = stdout.lock();
        writeln!(out, "{reply}").unwrap();
        out.flush().unwrap();
    }
    let _ = child.wait();
}

fn close_ends_process_tree() {
    let pid_file = std::env::temp_dir().join(format!("claude-code-agent-tree-{}.pid", std::process::id()));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (cli_pid, child_pid) = rt.block_on(async {
        let opts = SessionOptions {
            binary: std::env::current_exe().unwrap(),
            env: vec![
                (ROLE.into(), "cli".into()),
                (CHILD_PID_FILE.into(), pid_file.display().to_string()),
            ],
            ..Default::default()
        };
        let session = Session::spawn(opts, Initialize::default()).await.expect("spawn fake cli");
        let cli_pid = session.pid().expect("cli pid");
        let child_pid: u32 = wait_for(|| std::fs::read_to_string(&pid_file).ok()?.trim().parse().ok());
        assert!(alive(cli_pid) && alive(child_pid), "processes did not start");
        session.close().await;
        (cli_pid, child_pid)
    });
    let _ = std::fs::remove_file(&pid_file);
    wait_for(|| (!alive(cli_pid) && !alive(child_pid)).then_some(()));
    println!("process_tree: close() ended the CLI ({cli_pid}) and its child ({child_pid})");
}

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
    // Signal 0 checks existence; a reaped or never-reaped zombie of another
    // parent counts as gone once `ps` no longer lists it running.
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
