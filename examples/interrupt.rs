//! Interrupt a running turn from another task.
//!
//! Run: cargo run --example interrupt
use claude_sdk::{Agent, AgentEvent, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let opts = SessionOptions { include_partial: true, ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent.prompt("Write a 600-word story about a lighthouse keeper.").await?;

    // Interrupt from another task once ~300 characters have streamed.
    let mut chars = 0;
    let mut interrupted = false;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => {
                chars += t.len();
                print!("{t}");
                if chars > 300 && !interrupted {
                    interrupted = true;
                    let s = session.clone();
                    tokio::spawn(async move {
                        eprintln!("\n[interrupting]");
                        if let Err(e) = s.interrupt().await {
                            eprintln!("interrupt failed: {e}");
                        }
                    });
                }
            }
            AgentEvent::Done(r) => {
                println!("\n[turn ended: done, {chars} chars, {} ms]", r.duration_ms.unwrap_or(0));
                break;
            }
            AgentEvent::Error(e) => {
                println!("\n[turn ended: {e}, {chars} chars]");
                break;
            }
            _ => {}
        }
    }
    session.close().await;
    Ok(())
}
