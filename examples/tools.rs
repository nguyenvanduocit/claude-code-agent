//! Rust functions as Claude tools: #[claude_tool] + an in-process MCP server.
//! Claude sees them as `mcp__inventory__<name>`; calls run in this process.
//!
//! Run: cargo run --example tools --features macros
use claude_sdk::{claude_tool, Agent, AgentEvent, Initialize, SdkMcpServer, Session, SessionOptions};
use tokio_stream::StreamExt;

/// Look up how many units of a product are in stock
#[claude_tool]
async fn stock_level(sku: String) -> Result<u32, String> {
    match sku.as_str() {
        "WIDGET-1" => Ok(42),
        "GADGET-9" => Ok(0),
        other => Err(format!("unknown SKU {other}")),
    }
}

/// Convert an amount between currencies (supports USD and EUR)
#[claude_tool]
fn convert(amount: f64, from: String, to: String) -> Result<f64, String> {
    match (from.as_str(), to.as_str()) {
        ("USD", "EUR") => Ok(amount * 0.92),
        ("EUR", "USD") => Ok(amount / 0.92),
        (a, b) if a == b => Ok(amount),
        _ => Err("only USD and EUR are supported".into()),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server = SdkMcpServer::new("inventory").tool(stock_level_tool()).tool(convert_tool());
    let opts = SessionOptions { mcp_servers: vec![server], ..Default::default() };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent
        .prompt("Using the inventory tools: how many WIDGET-1 and GADGET-9 are in stock, what about NOPE-0, and what is 100 USD in EUR? One line each.")
        .await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::ToolUse { name, input, .. } => eprintln!("[tool] {name} {input}"),
            AgentEvent::Text(t) => print!("{t}"),
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    println!();
    session.close().await;
    Ok(())
}
