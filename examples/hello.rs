//! Minimal example: spawn a session, send one prompt, print the answer.
//!
//! Run: cargo run --example hello
use claude_code_agent::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use std::path::PathBuf;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = SessionOptions {
        binary: PathBuf::from("claude"),
        include_partial: true, // stream text chunks as they arrive
        ..Default::default()
    };

    eprintln!("Spawning session...");
    let session = Session::spawn(opts, Initialize::default()).await?;
    eprintln!("Session ID: {:?}", session.session_id());

    let agent = Agent::new(session);
    let mut stream = agent.prompt("say hi back in exactly one word").await?;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Text(t) => print!("{}", t),
            AgentEvent::Done(r) => {
                println!();
                eprintln!(
                    "cost=${:.6} ttft={}ms duration={}ms in={} out={}",
                    r.total_cost_usd.unwrap_or(0.0),
                    r.ttft_ms.unwrap_or(0),
                    r.duration_ms.unwrap_or(0),
                    r.input_tokens.unwrap_or(0),
                    r.output_tokens.unwrap_or(0),
                );
                break;
            }
            AgentEvent::Error(e) => {
                eprintln!("error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
