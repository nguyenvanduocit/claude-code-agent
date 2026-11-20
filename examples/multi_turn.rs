//! One persistent process, several turns; then close and resume the transcript
//! in a new process with Session::reconnect.
//!
//! Run: cargo run --example multi_turn
use claude_code_agent::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

async fn ask(agent: &Agent, prompt: &str) -> anyhow::Result<String> {
    let mut stream = agent.prompt(prompt).await?;
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done(r) => {
                eprintln!("  (cost ${:.4}, {} ms)", r.total_cost_usd.unwrap_or(0.0), r.duration_ms.unwrap_or(0));
                break;
            }
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    Ok(text)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let session = Session::spawn(SessionOptions::default(), Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    println!("1> {}", ask(&agent, "My favourite colour is teal. Reply only: noted").await?);
    println!("2> {}", ask(&agent, "And my favourite number is 7. Reply only: noted").await?);
    println!("3> {}", ask(&agent, "What are my favourite colour and number? One line.").await?);

    let id = session.session_id().unwrap_or_default().to_string();
    println!("session {id} closed with exit code {:?}", session.close().await);

    let resumed = Session::reconnect(&id, SessionOptions::default(), Initialize::default()).await?;
    let agent = Agent::new(resumed.clone());
    println!("4> {}", ask(&agent, "Remind me: what is my favourite colour? One word.").await?);
    resumed.close().await;
    Ok(())
}
