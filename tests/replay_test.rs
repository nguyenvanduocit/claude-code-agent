//! Replay tests — no CLI process, no API cost.
use claude_code_agent::replay::{replay, Recording};
use claude_code_agent::{Agent, AgentEvent, SessionOptions, TurnResult};
use tokio_stream::StreamExt;

/// Turn 1 is prompted; the CLI then starts a turn by itself (`origin` set, as
/// after a background task finishes, text "auto") before prompted turn 2.
const RECORDING: &str = r#"{"type":"control_response","response":{"subtype":"success","request_id":"req_0","response":{"pid":1}}}
{"type":"system","subtype":"init","session_id":"s1","model":"m"}
{"type":"user","isReplay":true,"uuid":"rec-1","message":{"role":"user","content":"first"},"session_id":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"one"}]},"session_id":"s1"}
{"type":"result","subtype":"success","session_id":"s1","total_cost_usd":0.1}
{"type":"system","subtype":"background_tasks_changed","tasks":[],"session_id":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"auto"}]},"session_id":"s1"}
{"type":"result","subtype":"success","session_id":"s1","total_cost_usd":0.2,"origin":{"kind":"task-notification"}}
{"type":"user","isReplay":true,"uuid":"rec-2","message":{"role":"user","content":"second"},"session_id":"s1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"two"}]},"session_id":"s1"}
{"type":"result","subtype":"success","session_id":"s1","total_cost_usd":0.3}
{"type":"system","subtype":"compact_boundary","session_id":"s1","compact_metadata":{"trigger":"manual","pre_tokens":900}}
{"type":"user","isReplay":true,"uuid":"rec-3","message":{"role":"user","content":"/compact"},"session_id":"s1"}
{"type":"result","subtype":"success","session_id":"s1","total_cost_usd":0.4}
"#;

async fn turn(agent: &Agent, prompt: &str) -> (String, TurnResult) {
    let mut stream = agent.prompt(prompt).await.expect("send");
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done(r) => return (text, r),
            AgentEvent::Error(e) => panic!("turn failed: {e}"),
            _ => {}
        }
    }
    panic!("stream ended without a result");
}

#[test]
fn cli_started_turn_stays_with_next_prompted_turn() {
    let rec = Recording::parse(RECORDING).expect("parse");
    assert_eq!(rec.turns.len(), 3);
    assert_eq!(rec.initialize_response["pid"], 1);
}

#[tokio::test]
async fn prompt_stream_holds_only_its_own_turn() {
    let path = std::env::temp_dir().join(format!("claude-code-agent-replay-{}.jsonl", std::process::id()));
    std::fs::write(&path, RECORDING).expect("write recording");

    let session = replay(&path, SessionOptions::default()).await.expect("replay");
    assert_eq!(session.initialize_response().map(|r| r["pid"].clone()), Some(1.into()));
    let agent = Agent::new(session);

    let (text, r1) = turn(&agent, "first").await;
    assert_eq!((text.as_str(), r1.total_cost_usd), ("one", Some(0.1)));

    let (text, r2) = turn(&agent, "second").await;
    assert_eq!((text.as_str(), r2.total_cost_usd), ("two", Some(0.3)));
    assert_eq!(agent.session().session_id().as_deref(), Some("s1"));

    // Slash command: the CLI echoes the prompt after the command's frames.
    let mut stream = agent.prompt("/compact").await.expect("send");
    let mut compacted = None;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Compacted { trigger, pre_tokens, .. } => compacted = Some((trigger, pre_tokens)),
            AgentEvent::Done(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }
    assert_eq!(compacted, Some(("manual".to_string(), Some(900))));
    let _ = std::fs::remove_file(&path);
}
