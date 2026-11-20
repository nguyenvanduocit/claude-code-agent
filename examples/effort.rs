//! Start a session at one model and effort level, then switch both mid-session.
//!
//! Run: cargo run --example effort
use claude_sdk::protocol::Effort;
use claude_sdk::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

async fn ask(agent: &Agent, prompt: &str) -> anyhow::Result<()> {
    let mut stream = agent.prompt(prompt).await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => print!("{t}"),
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    println!();
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = SessionOptions {
        model: Some("haiku".into()),
        effort: Some(Effort::Low),
        ..Default::default()
    };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    ask(&agent, "Reply with one word: ready").await?;

    session.set_model(Some("sonnet")).await?;
    session.set_effort(Some(Effort::High)).await?;
    println!("[switched to sonnet, effort high]");
    ask(&agent, "Reply with one word: switched").await?;

    session.set_effort(None).await?;
    println!("[effort override cleared: CLI default level]");
    Ok(())
}
