//! Record a live session once, then replay it with no process and no API cost
//! (useful for deterministic tests of code built on the SDK).
//!
//! Run: cargo run --example record_replay
use claude_sdk::{replay, Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

async fn ask(agent: &Agent, prompt: &str) -> anyhow::Result<(String, Option<f64>)> {
    let mut stream = agent.prompt(prompt).await?;
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done(r) => return Ok((text, r.total_cost_usd)),
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    anyhow::bail!("stream ended without a result")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::temp_dir().join("claude-sdk-example.jsonl");

    let live = Session::spawn(
        SessionOptions { record: Some(path.clone()), ..Default::default() },
        Initialize::default(),
    )
    .await?;
    let (text, cost) = ask(&Agent::new(live.clone()), "Name one prime number between 10 and 20. Number only.").await?;
    println!("live:     {text:?} cost={cost:?}");
    live.close().await;

    let t = std::time::Instant::now();
    let replayed = replay::replay(&path, SessionOptions::default()).await?;
    let (text, cost) = ask(&Agent::new(replayed), "anything — the recording answers").await?;
    println!("replayed: {text:?} cost={cost:?} in {:?} (no API call)", t.elapsed());
    std::fs::remove_file(&path)?;
    Ok(())
}
