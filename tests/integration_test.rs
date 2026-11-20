//! End-to-end tests against the real `claude` binary (needs credentials).
//! Run: cargo test --test integration_test -- --ignored --nocapture
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use claude_sdk::protocol::{HookEventKind, HookOutput, PermissionMode, ToolDecision};
use claude_sdk::{
    Agent, AgentEvent, CanUseTool, HookRegistry, Initialize, SdkMcpServer, Session, SessionOptions,
    TurnResult,
};
use tokio_stream::StreamExt;

/// Run one turn; return (text, tool names used, result).
async fn turn(agent: &Agent, prompt: &str) -> (String, Vec<String>, TurnResult) {
    let mut stream = agent.prompt(prompt).await.expect("send prompt");
    let (mut text, mut tools) = (String::new(), vec![]);
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::ToolUse { name, .. } => tools.push(name),
            AgentEvent::Done(r) => return (text, tools, r),
            AgentEvent::Error(e) => panic!("turn failed: {e}"),
            _ => {}
        }
    }
    panic!("stream ended without a result");
}

#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn prompt_round_trip() {
    let opts = SessionOptions { include_partial: true, ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);

    let (text, _, result) = turn(&agent, "say hi back in exactly one word, nothing else").await;

    eprintln!("text={text:?} cost=${:.4}", result.total_cost_usd.unwrap_or(0.0));
    assert!(!text.trim().is_empty(), "no text received");
    assert!(agent.session().session_id().is_some(), "session id not captured");
}

/// An SDK hook blocks Bash; a can_use_tool handler denies Write.
/// Neither tool may take effect, and both callbacks must have run.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn hook_and_permission_callbacks_gate_tools() {
    let dir = std::env::temp_dir().join(format!("claude-sdk-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let marker = dir.join("marker.txt");

    let hook_calls = Arc::new(AtomicUsize::new(0));
    let perm_calls = Arc::new(AtomicUsize::new(0));

    let mut hooks = HookRegistry::new();
    let hc = hook_calls.clone();
    hooks.on(HookEventKind::PreToolUse, Some("Bash"), move |p| {
        hc.fetch_add(1, Ordering::SeqCst);
        async move {
            assert_eq!(p.tool_name.as_deref(), Some("Bash"));
            HookOutput::block("Bash is disabled by the SDK hook")
        }
    });

    let pc = perm_calls.clone();
    let can_use_tool = CanUseTool::new(move |req| {
        pc.fetch_add(1, Ordering::SeqCst);
        async move { ToolDecision::deny(format!("{} denied by SDK handler", req.tool_name)) }
    });

    let opts = SessionOptions {
        cwd: Some(dir.clone()),
        permission_mode: Some(PermissionMode::Default),
        hooks,
        can_use_tool: Some(can_use_tool),
        ..Default::default()
    };
    let session = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);

    let (text, tools, _) = turn(
        &agent,
        &format!(
            "Do both steps, one tool call each, no retries with other tools: \
             1) run `touch {m}` with the Bash tool; \
             2) create {m} with the Write tool containing 'x'. \
             Then report what happened in one sentence.",
            m = marker.display()
        ),
    )
    .await;
    eprintln!("tools={tools:?}\ntext={text}");

    assert!(tools.iter().any(|t| t == "Bash"), "model never called Bash");
    assert!(hook_calls.load(Ordering::SeqCst) >= 1, "PreToolUse hook never fired");
    assert!(perm_calls.load(Ordering::SeqCst) >= 1, "can_use_tool never fired");
    assert!(!marker.exists(), "a gated tool still created the file");
    let _ = std::fs::remove_dir_all(&dir);
}

/// close() ends the process; reconnect() resumes the transcript.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn close_and_reconnect_keeps_transcript() {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session.clone());
    turn(&agent, "Remember the codeword PELICAN-42. Reply only: ok").await;
    let id = session.session_id().expect("session id").to_string();

    let pid = session.initialize_response().and_then(|r| r["pid"].as_i64()).expect("pid");
    let code = session.close().await;
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(!alive, "CLI pid {pid} still running after close() (exit code {code:?})");

    let resumed = Session::reconnect(&id, SessionOptions::default(), Initialize::default())
        .await
        .expect("reconnect");
    let (text, _, _) = turn(&Agent::new(resumed), "What was the codeword? Reply with it only.").await;
    eprintln!("resumed text={text:?}");
    assert!(text.contains("PELICAN-42"), "transcript not resumed");
}

/// Look up a secret only this process knows, so the answer proves the tool ran.
#[claude_sdk_macros::claude_tool]
async fn vault_lookup(key: String) -> Result<String, String> {
    match key.as_str() {
        "launch" => Ok("ZEBRA-7731".into()),
        other => Err(format!("unknown key {other}")),
    }
}

/// Claude calls an in-process #[claude_tool] function through the SDK MCP bridge.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn sdk_mcp_tool_is_called_by_claude() {
    let opts = SessionOptions {
        mcp_servers: vec![SdkMcpServer::new("vault").tool(vault_lookup_tool())],
        ..Default::default()
    };
    let session = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);

    let (text, tools, _) = turn(
        &agent,
        "Use the vault_lookup tool with key \"launch\" and reply with the value only.",
    )
    .await;
    eprintln!("tools={tools:?} text={text:?}");

    assert!(tools.iter().any(|t| t == "mcp__vault__vault_lookup"), "tool not called");
    assert!(text.contains("ZEBRA-7731"), "tool result not used");
}

/// /compact with instructions: PreCompact hook sees them, the stream reports
/// Compacted, and the session keeps working afterwards.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn compact_with_instructions() {
    let seen = Arc::new(std::sync::Mutex::new(None::<serde_json::Value>));
    let mut hooks = HookRegistry::new();
    let s = seen.clone();
    hooks.on(HookEventKind::PreCompact, None, move |p| {
        if let Ok(mut g) = s.lock() {
            *g = p.extra.get("custom_instructions").cloned();
        }
        async { HookOutput::allow() }
    });
    let session = Session::spawn(SessionOptions { hooks, ..Default::default() }, Initialize::default())
        .await
        .expect("spawn");
    let agent = Agent::new(session);
    turn(&agent, "Remember the codeword OSPREY-9. Reply only: ok").await;

    let mut stream = claude_sdk::compaction::compact(&agent, Some("keep the codeword"))
        .await
        .expect("compact");
    let mut compacted = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Compacted { trigger, pre_tokens, .. } => compacted = Some((trigger, pre_tokens)),
            AgentEvent::Done(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }
    eprintln!("compacted={compacted:?} hook_saw={:?}", seen.lock().map(|g| g.clone()));
    let (trigger, _) = compacted.expect("no Compacted event");
    assert_eq!(trigger, "manual");
    assert_eq!(
        seen.lock().ok().and_then(|g| g.clone()),
        Some(serde_json::json!("keep the codeword"))
    );

    let (text, _, _) = turn(&agent, "What was the codeword? Reply with it only.").await;
    assert!(text.contains("OSPREY-9"), "codeword lost after compaction: {text:?}");
}

/// A recorded live session replays to the same events without a process.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn record_then_replay() {
    let path = std::env::temp_dir().join(format!("claude-sdk-rec-{}.jsonl", std::process::id()));
    let opts = SessionOptions { record: Some(path.clone()), ..Default::default() };
    let live = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let (live_text, _, live_result) = turn(&Agent::new(live.clone()), "Reply with exactly: replay works").await;
    live.close().await;

    let replayed = claude_sdk::replay::replay(&path, SessionOptions::default()).await.expect("replay");
    assert_eq!(replayed.initialize_response(), live.initialize_response());
    let agent = Agent::new(replayed);
    let (text, _, result) = turn(&agent, "anything").await;
    assert_eq!(text, live_text);
    assert_eq!(result.total_cost_usd, live_result.total_cost_usd);

    // No recorded turn left → the stream reports an error instead of hanging.
    let mut stream = agent.prompt("again").await.expect("send");
    let mut last = None;
    while let Some(ev) = stream.next().await {
        last = Some(ev);
    }
    assert!(matches!(last, Some(AgentEvent::Error(_))), "got {last:?}");
    let _ = std::fs::remove_file(&path);
}

/// background_tasks_changed snapshots decode into BackgroundTask.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn background_task_snapshots_decode() {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);
    let mut stream = agent
        .prompt("Run `sleep 3` with the Bash tool using run_in_background: true, then reply only: started")
        .await
        .expect("send");
    let mut tasks = vec![];
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::BackgroundTasks(t) => tasks.extend(t),
            AgentEvent::Done(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }
    eprintln!("tasks={tasks:?}");
    assert!(tasks.iter().any(|t| !t.task_id.is_empty() && t.task_type.contains("bash")), "no bash task seen");
}

/// Two prompts sent back to back on one session: the CLI queues the second,
/// and each stream must hold only its own turn.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn concurrent_prompts_get_their_own_turns() {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);
    let s1 = agent.prompt("Reply with exactly: alpha").await.expect("send 1");
    let s2 = agent.prompt("Reply with exactly: bravo").await.expect("send 2");
    let collect = |mut s: claude_sdk::agent::AgentStream| async move {
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            match ev {
                AgentEvent::Text(t) => text.push_str(&t),
                AgentEvent::Done(_) | AgentEvent::Error(_) => break,
                _ => {}
            }
        }
        text
    };
    let (t1, t2) = tokio::join!(collect(s1), collect(s2));
    eprintln!("t1={t1:?} t2={t2:?}");
    assert!(t1.contains("alpha") && !t1.contains("bravo"), "stream 1: {t1:?}");
    assert!(t2.contains("bravo") && !t2.contains("alpha"), "stream 2: {t2:?}");
}

/// A background Bash task keeps the CLI alive after stdin closes; close() must
/// still end the CLI and the task's processes (they share its process group).
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn close_ends_cli_and_background_children() {
    let secs = 400 + std::process::id() % 100;
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session.clone());
    let prompt = format!("Run `sleep {secs}` with the Bash tool using run_in_background: true, then reply only: started");
    let _ = turn(&agent, &prompt).await;
    let sleepers = || {
        let out = std::process::Command::new("pgrep").args(["-f", &format!("sleep {secs}")]).output().expect("pgrep");
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_owned).collect::<Vec<_>>()
    };
    assert!(!sleepers().is_empty(), "background sleep never started");

    let started = std::time::Instant::now();
    let code = session.close().await;
    eprintln!("close took {:?}, exit={code:?}", started.elapsed());
    assert!(!session.is_alive(), "CLI still alive after close");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(sleepers().is_empty(), "background children survived: {:?}", sleepers());
}

/// A failed API call arrives as `subtype:"success", is_error:true`; the
/// stream must end with Error carrying the CLI's message, not Done.
#[tokio::test]
#[ignore = "requires a real claude binary"]
async fn api_error_ends_turn_with_error() {
    let opts = SessionOptions { model: Some("claude-nonexistent-9".into()), ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let mut stream = Agent::new(session).prompt("hi").await.expect("send");
    let mut outcome = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Done(_) => outcome = Some(Err("Done".to_string())),
            AgentEvent::Error(e) => outcome = Some(Ok(e)),
            _ => {}
        }
    }
    let message = outcome.expect("no result").expect("turn reported Done");
    eprintln!("error={message:?}");
    assert!(message.contains("claude-nonexistent-9"), "{message}");
}

/// `code_mode` runs a patched CLI copy: the REPL tool exists and Claude uses it.
#[tokio::test]
#[ignore = "requires a real claude binary and valid credentials"]
async fn code_mode_exposes_repl() {
    let opts = SessionOptions { code_mode: true, setting_sources: Some(vec![]), ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await.expect("spawn");
    let agent = Agent::new(session);
    let (text, tools, _) = turn(&agent, "Use your REPL tool to compute 6*7 in JavaScript. Reply with just the number.").await;
    eprintln!("tools={tools:?} text={text:?}");
    assert!(tools.iter().any(|t| t == "REPL"), "REPL not used: {tools:?}");
    assert!(text.contains("42"), "{text}");
}
