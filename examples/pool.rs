//! Session pool: pre-warmed processes, parallel prompts, idle reuse.
//! A reused session is reset with /clear, so tasks never share a conversation.
//!
//! Run: cargo run --example pool
use claude_sdk::{Agent, AgentEvent, PoolConfig, SessionOptions, SessionPool};
use tokio_stream::StreamExt;

async fn run(pool: &SessionPool, prompt: &str) -> anyhow::Result<String> {
    let session = pool.acquire().await?; // returns to the pool on drop
    let agent = Agent::new(session.session.clone());
    let mut stream = agent.prompt(prompt).await?;
    let mut text = String::new();
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => text.push_str(&t),
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    Ok(text)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let pool = SessionPool::new(PoolConfig {
        max_sessions: 3,
        options: SessionOptions { setting_sources: Some(vec![]), ..Default::default() },
    });
    let t = std::time::Instant::now();
    let warmed = pool.warm(3).await;
    println!("warmed {warmed} sessions in {:?}", t.elapsed());

    let t = std::time::Instant::now();
    let (a, b, c) = tokio::join!(
        run(&pool, "Capital of France? One word."),
        run(&pool, "Capital of Japan? One word."),
        run(&pool, "Capital of Peru? One word."),
    );
    println!("parallel answers in {:?}: {} / {} / {}", t.elapsed(), a?, b?, c?);

    // The reused session starts empty: it cannot know the earlier answers.
    let t = std::time::Instant::now();
    let answer = run(&pool, "Which capitals did I ask about earlier? If none, reply only: NONE").await?;
    println!("reused idle session after /clear: {answer} in {:?}", t.elapsed());
    Ok(())
}
