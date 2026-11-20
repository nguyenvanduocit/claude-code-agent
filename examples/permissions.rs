//! Tool permissions: decide in Rust whenever Claude Code would ask the user.
//! Denies file writes and rewrites Bash commands to add a prefix.
//! Read-only commands (ls, uname, …) never ask, so the demo uses `mkdir`.
//!
//! Run: cargo run --example permissions
use claude_sdk::protocol::{PermissionMode, ToolDecision};
use claude_sdk::{Agent, AgentEvent, CanUseTool, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let can_use_tool = CanUseTool::new(|req| async move {
        eprintln!("[can_use_tool] {} {}", req.tool_name, req.input);
        match req.tool_name.as_str() {
            "Write" | "Edit" => ToolDecision::deny("This session is read-only."),
            "Bash" => {
                // Rewrite the input before it runs.
                let mut input = req.input.clone();
                let cmd = input["command"].as_str().unwrap_or("").to_string();
                input["command"] = format!("echo '[sandboxed]' && {cmd}").into();
                ToolDecision::Allow { updated_input: Some(input) }
            }
            _ => ToolDecision::allow(),
        }
    });

    let opts = SessionOptions {
        // "default" makes the CLI ask for Bash/Write, so the handler is consulted.
        permission_mode: Some(PermissionMode::Default),
        can_use_tool: Some(can_use_tool),
        cwd: Some(std::env::temp_dir()),
        // Ignore the user's own permission rules so the handler decides.
        setting_sources: Some(vec![]),
        ..Default::default()
    };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent
        .prompt("Run `mkdir -p perm-demo-dir && echo created` with Bash and show its full output. Then write 'hi' to ./perm-demo.txt with the Write tool. Report both results briefly.")
        .await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::Text(t) => print!("{t}"),
            AgentEvent::Done(_) => break,
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    println!();
    session.close().await;
    let _ = std::fs::remove_dir(std::env::temp_dir().join("perm-demo-dir"));
    Ok(())
}
