//! Watch every turn: tool results, subagent events, and the turn the CLI
//! starts on its own when a background command finishes.
//!
//! Run: cargo run --example watch
use claude_sdk::{Agent, AgentEvent, Initialize, PermissionMode, Session, SessionOptions};
use tokio_stream::StreamExt;

fn show(prefix: &str, ev: &AgentEvent) {
    match ev {
        AgentEvent::ToolUse { name, input, .. } => println!("{prefix}[tool] {name} {input}"),
        AgentEvent::ToolResult { content, structured, .. } => {
            println!("{prefix}[result] {content}");
            if let Some(s) = structured {
                println!("{prefix}[structured] {s}");
            }
        }
        AgentEvent::Subagent { parent_tool_use_id, event } => show(&format!("{prefix}[sub {parent_tool_use_id}] "), event),
        AgentEvent::Text(t) => println!("{prefix}{t}"),
        AgentEvent::Done(r) => println!("{prefix}[done] origin={:?}", r.origin),
        AgentEvent::Error(e) => println!("{prefix}[error] {e}"),
        _ => {}
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = SessionOptions { permission_mode: Some(PermissionMode::BypassPermissions), ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());
    let mut all = agent.watch();

    // The prompted turn ends quickly; the background command's completion
    // makes the CLI start a second turn, which only `watch` sees.
    let mut turn = agent
        .prompt("Start the bash command `sleep 3 && echo finished` with run_in_background true, then reply 'started'.")
        .await?;
    while let Some(ev) = turn.next().await {
        if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) {
            break;
        }
    }

    let mut finished = 0;
    while let Some(ev) = all.next().await {
        show("", &ev);
        if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) {
            finished += 1;
            if finished == 2 {
                break;
            }
        }
    }
    session.close().await;
    Ok(())
}
