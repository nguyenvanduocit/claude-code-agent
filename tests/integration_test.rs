use claude_sdk::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use std::path::PathBuf;
use tokio_stream::StreamExt;

fn claude_binary() -> PathBuf {
    // prefer the system claude in PATH
    if let Ok(p) = which_claude() {
        return p;
    }
    // fall back to versioned installs, pick newest
    let versions_dir = dirs_home().join(".local/share/claude/versions");
    if let Ok(mut entries) = std::fs::read_dir(&versions_dir) {
        let mut versions: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        versions.sort_by_key(|e| e.file_name());
        for entry in versions.into_iter().rev() {
            let bin = entry.path().join("claude");
            if bin.exists() {
                return bin;
            }
        }
    }
    PathBuf::from("claude")
}

fn which_claude() -> Result<PathBuf, ()> {
    std::process::Command::new("which")
        .arg("claude")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let s = String::from_utf8(o.stdout).ok()?;
            let p = PathBuf::from(s.trim());
            if p.exists() { Some(p) } else { None }
        })
        .ok_or(())
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp"))
}

#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn test_prompt_hi_end_to_end() {
    let binary = claude_binary();
    eprintln!("Using claude binary: {}", binary.display());

    let mut opts = SessionOptions::default();
    opts.binary = binary;

    let init = Initialize::new(
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
            .to_string(),
    );

    let session = Session::spawn(opts, init)
        .await
        .expect("failed to spawn session");

    eprintln!("Session ID: {:?}", session.session_id());

    let agent = Agent::new(session);
    let mut stream = agent
        .prompt("say hi back in exactly one word, nothing else")
        .await
        .expect("failed to send prompt");

    let mut text = String::new();
    let mut got_done = false;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text(t) => {
                eprint!("{}", t);
                text.push_str(&t);
            }
            AgentEvent::Thinking(t) => eprintln!("[thinking: {}]", t),
            AgentEvent::ToolUse { name, .. } => eprintln!("[tool: {}]", name),
            AgentEvent::BackgroundTasks(tasks) => {
                eprintln!("[background tasks: {}]", tasks.len())
            }
            AgentEvent::ModelFallback { from, to } => {
                eprintln!("[model fallback: {:?} → {}]", from, to)
            }
            AgentEvent::StreamLag { dropped } => {
                eprintln!("[stream lag: {} dropped]", dropped)
            }
            AgentEvent::Done(result) => {
                eprintln!("\n--- turn complete ---");
                eprintln!(
                    "cost:     ${:.6}",
                    result.total_cost_usd.unwrap_or(0.0)
                );
                eprintln!("ttft:     {}ms", result.ttft_ms.unwrap_or(0));
                eprintln!("duration: {}ms", result.duration_ms.unwrap_or(0));
                eprintln!(
                    "tokens:   in={} out={}",
                    result.input_tokens.unwrap_or(0),
                    result.output_tokens.unwrap_or(0)
                );
                got_done = true;
                break;
            }
            AgentEvent::Error(e) => {
                panic!("agent error: {}", e);
            }
        }
    }

    assert!(got_done, "stream ended without Done event");
    assert!(!text.trim().is_empty(), "no text received from agent");
    eprintln!("\nreceived text: {:?}", text.trim());
}
