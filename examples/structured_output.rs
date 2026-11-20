//! Structured output: the turn's answer as JSON matching a schema.
//!
//! Run: cargo run --example structured_output
use claude_sdk::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let init = Initialize {
        json_schema: Some(serde_json::json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" },
                "population_millions": { "type": "number" }
            },
            "required": ["city", "population_millions"]
        })),
        ..Default::default()
    };
    let session = Session::spawn(SessionOptions::default(), init).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent.prompt("What is the largest city in Japan?").await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Done(r) => {
                println!("structured: {}", r.structured_output.unwrap_or_default());
                println!("turns: {:?}, cost: ${:.4}", r.num_turns, r.total_cost_usd.unwrap_or(0.0));
                break;
            }
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    session.close().await;
    Ok(())
}
