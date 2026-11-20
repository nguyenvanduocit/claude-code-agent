//! Register an SDK callback on all 33 hook events, drive a session through
//! tool calls, a failing tool, a subagent, a cd and a compaction, then print
//! which events reached Rust.
//!
//! Run: cargo run --example hook_coverage
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use claude_sdk::protocol::{HookEventKind, HookOutput, PermissionMode};
use claude_sdk::{compaction, Agent, AgentEvent, HookRegistry, Initialize, Session, SessionOptions};
use tokio_stream::StreamExt;

const ALL: [HookEventKind; 33] = {
    use HookEventKind::*;
    [
        PreToolUse, PostToolUse, PostToolUseFailure, PostToolBatch, Notification, UserPromptSubmit,
        UserPromptExpansion, SessionStart, SessionEnd, Stop, StopFailure, SubagentStart, SubagentStop,
        PreCompact, PostCompact, PreModelSwitch, PostModelSwitch, PermissionRequest, PermissionDenied,
        Setup, TeammateIdle, TaskCreated, TaskCompleted, Elicitation, ElicitationResult, ConfigChange,
        WorktreeCreate, WorktreeRemove, InstructionsLoaded, CwdChanged, FileChanged, DirectoryAdded,
        MessageDisplay,
    ]
};

async fn run(agent: &Agent, prompt: &str) -> anyhow::Result<()> {
    let mut s = agent.prompt(prompt).await?;
    while let Some(ev) = s.next().await {
        match ev {
            AgentEvent::Done(_) => return Ok(()),
            AgentEvent::Error(e) => anyhow::bail!(e),
            _ => {}
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fired: Arc<Mutex<BTreeMap<String, usize>>> = Arc::default();
    let mut hooks = HookRegistry::new();
    for event in ALL {
        let fired = fired.clone();
        hooks.on(event.clone(), None, move |p| {
            if let Ok(mut f) = fired.lock() {
                *f.entry(format!("{:?}", p.hook_event_name)).or_default() += 1;
            }
            async { HookOutput::allow() }
        });
    }

    let dir = std::env::temp_dir().join("claude-sdk-hook-coverage");
    std::fs::create_dir_all(dir.join("sub"))?;
    let opts = SessionOptions {
        hooks,
        cwd: Some(dir.clone()),
        setting_sources: Some(vec![]),
        permission_mode: Some(PermissionMode::BypassPermissions),
        ..Default::default()
    };
    let session = Session::spawn(opts, Initialize::default()).await?;
    let agent = Agent::new(session.clone());

    run(&agent, "Run `echo one` and `cat /nonexistent-file` with Bash (two separate calls), then reply: ok").await?;
    run(&agent, "Use the Agent tool to spawn a general-purpose subagent that replies with the word pong. Then reply: ok").await?;
    run(&agent, "Run `cd sub` with Bash, then reply: ok").await?;
    let mut s = compaction::compact(&agent, Some("keep it short")).await?;
    while let Some(ev) = s.next().await {
        if matches!(ev, AgentEvent::Done(_) | AgentEvent::Error(_)) { break; }
    }
    session.close().await;

    let fired = fired.lock().map(|f| f.clone()).unwrap_or_default();
    for event in ALL {
        let name = format!("{event:?}");
        match fired.get(&name) {
            Some(n) => println!("  fired  {name:<20} x{n}"),
            None => println!("  -      {name}"),
        }
    }
    println!("{}/33 events reached SDK callbacks", fired.len());
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
