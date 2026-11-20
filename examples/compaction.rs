//! Compact the context on demand with custom instructions.
//!
//! Run: cargo run --example compaction
use claude_code_agent::protocol::{HookEventKind, HookOutput};
use claude_code_agent::{compaction, Agent, AgentEvent, HookRegistry, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut hooks = HookRegistry::new();
    hooks.on(HookEventKind::PreCompact, None, |p| async move {
        eprintln!("[PreCompact] instructions = {:?}", p.extra.get("custom_instructions"));
        HookOutput::allow()
    });
    hooks.on(HookEventKind::PostCompact, None, |p| async move {
        let summary = p.extra.get("compact_summary").and_then(|v| v.as_str()).unwrap_or("");
        eprintln!("[PostCompact] summary: {} chars", summary.len());
        HookOutput::allow()
    });

    let session = Session::spawn(SessionOptions { hooks, ..Default::default() }, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut s = agent.prompt("The release codename is HERON. Reply only: ok").await?;
    while let Some(ev) = s.next().await {
        if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
    }

    let mut s = compaction::compact(&agent, Some("Keep the release codename")).await?;
    while let Some(ev) = s.next().await {
        match ev {
            AgentEvent::Compacted { trigger, pre_tokens, post_tokens } => {
                println!("compacted ({trigger}): {pre_tokens:?} -> {post_tokens:?} tokens")
            }
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }

    let mut s = agent.prompt("What is the release codename? One word.").await?;
    while let Some(ev) = s.next().await {
        match ev {
            AgentEvent::Text(t) => print!("after compaction: {t}"),
            AgentEvent::Done(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }
    println!();
    session.close().await;
    Ok(())
}
