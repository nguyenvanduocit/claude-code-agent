//! Hooks: async Rust closures on Claude Code hook events.
//!   - PreToolUse  (matcher "Bash"): block commands containing `rm`
//!   - UserPromptSubmit: inject extra context into every prompt
//!   - PostToolUse: observe every finished tool call
//!   - ToolAllowList: block every tool except Read/Bash/Glob
//!
//! Run: cargo run --example hooks
use claude_sdk::middleware::ToolAllowList;
use claude_sdk::protocol::{HookEventKind, HookOutput};
use claude_sdk::{Agent, AgentEvent, HookRegistry, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Hooks are registered BEFORE spawn: the CLI learns them at initialize.
    let mut hooks = HookRegistry::new();

    hooks.on(HookEventKind::PreToolUse, Some("Bash"), |p| async move {
        let cmd = p.tool_input.as_ref().and_then(|i| i["command"].as_str()).unwrap_or("").to_string();
        eprintln!("[PreToolUse] Bash: {cmd}");
        if cmd.contains("rm ") {
            HookOutput::block("Deleting files is not allowed in this session.")
        } else {
            HookOutput::allow()
        }
    });

    hooks.on(HookEventKind::UserPromptSubmit, None, |_| async {
        HookOutput::with_context(HookEventKind::UserPromptSubmit, "Always answer in English, in at most 2 sentences.")
    });

    hooks.on(HookEventKind::PostToolUse, None, |p| async move {
        eprintln!("[PostToolUse] {} finished", p.tool_name.unwrap_or_default());
        HookOutput::allow()
    });

    ToolAllowList::new(["Read", "Bash", "Glob"]).install(&mut hooks);

    let session = Session::spawn(SessionOptions { hooks, ..Default::default() }, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    let mut stream = agent
        .prompt("First run `echo hello` with Bash. Then run `rm -f /tmp/does-not-matter` with Bash. Then try the WebFetch tool on https://example.com. Report what happened.")
        .await?;
    while let Some(ev) = stream.next().await {
        match ev {
            AgentEvent::ToolUse { name, .. } => eprintln!("[tool call] {name}"),
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
