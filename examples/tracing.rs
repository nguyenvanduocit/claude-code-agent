//! Observability: the SDK emits `tracing` spans claude.turn / claude.hook /
//! claude.tool. Any subscriber works; this prints them to stderr. For
//! OpenTelemetry, add a `tracing-opentelemetry` layer instead.
//!
//! Run: cargo run --example tracing
use claude_sdk::protocol::{HookEventKind, HookOutput};
use claude_sdk::{Agent, AgentEvent, HookRegistry, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;
use tracing_subscriber::fmt::format::FmtSpan;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("claude_sdk=info")
        .with_span_events(FmtSpan::CLOSE) // one line per finished span, with its duration
        .with_writer(std::io::stderr)
        .init();

    let mut hooks = HookRegistry::new();
    hooks.on(HookEventKind::PreToolUse, None, |_| async { HookOutput::allow() });
    let session = Session::spawn(SessionOptions { hooks, ..Default::default() }, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent.prompt("Run `date` with Bash and reply with the year only.").await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => println!("{t}"),
            AgentEvent::Done(_) | AgentEvent::Error(_) => break,
            _ => {}
        }
    }
    drop(stream); // closes the claude.turn span
    session.close().await;
    Ok(())
}
