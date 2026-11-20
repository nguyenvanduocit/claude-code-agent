//! Manual context compaction with custom instructions.
//!
//! `/compact <instructions>` sent as a user turn makes the CLI summarize the
//! conversation, steering the summary with `instructions`. The turn's stream
//! yields `AgentEvent::Compacted` and ends with `Done`.
//!
//! Fails with `AgentError::CommandUnavailable` when the CLI does not offer
//! `/compact` (`SessionOptions::disable_slash_commands`): the text would
//! reach the model as an ordinary prompt.
//!
//! Hooks see it too: `PreCompact` receives `trigger: "manual"` and
//! `custom_instructions`; `PostCompact` receives `compact_summary`.
//! Source: binary `PreCompact` / `PostCompact` hook input schemas,
//!         `system/compact_boundary` schema
use crate::agent::{Agent, AgentError, AgentStream};

/// Compact the session's context now.
pub async fn compact(agent: &Agent, instructions: Option<&str>) -> Result<AgentStream, AgentError> {
    if !agent.session().offers_command("compact") {
        return Err(AgentError::CommandUnavailable("compact".into()));
    }
    let command = match instructions {
        Some(i) if !i.trim().is_empty() => format!("/compact {}", i.trim()),
        _ => "/compact".to_string(),
    };
    agent.prompt(command).await
}
