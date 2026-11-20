//! Code Mode: Claude gets the `REPL` tool and works by writing JavaScript
//! that calls its tools. The SDK runs a patched copy of the CLI (built once,
//! cached under ~/.cache/claude-code-agent/code-mode/).
//!
//! Run: cargo run --example code_mode
use claude_code_agent::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    let opts = SessionOptions { code_mode: true, setting_sources: Some(vec![]), ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await?;
    println!("spawned in {:?}", started.elapsed());

    let agent = Agent::new(session.clone());
    let mut stream = agent
        .prompt("With one REPL call, count the .rs files under ./src and sum their line counts. Reply: <files> files, <lines> lines")
        .await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::ToolUse { name, input, .. } => {
                let code = input.get("code").and_then(|c| c.as_str()).unwrap_or_default();
                println!("[{name}] {}", code.lines().next().unwrap_or_default());
            }
            AgentEvent::Text(t) => println!("claude: {t}"),
            AgentEvent::Done(r) => println!("cost=${:.4}", r.total_cost_usd.unwrap_or(0.0)),
            AgentEvent::Error(e) => println!("error: {e}"),
            _ => {}
        }
    }
    session.close().await;
    Ok(())
}
