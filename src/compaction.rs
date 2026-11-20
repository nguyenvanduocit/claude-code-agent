//! Manual context compaction with custom instructions.
//!
//! `/compact <instructions>` sent as a user turn makes the CLI summarize the
//! conversation, steering the summary with `instructions`. The turn's stream
//! yields `AgentEvent::Compacted` and ends with `Done`.
//!
//! Hooks see it too: PreCompact receives `trigger: "manual"` and
//! `custom_instructions`; PostCompact receives `compact_summary`.
//! Source: binary PreCompact / PostCompact hook input schemas,
//!         system/compact_boundary schema
use crate::agent::{Agent, AgentError, AgentStream};

/// Compact the session's context now.
pub async fn compact(agent: &Agent, instructions: Option<&str>) -> Result<AgentStream, AgentError> {
    let command = match instructions {
        Some(i) if !i.trim().is_empty() => format!("/compact {}", i.trim()),
        _ => "/compact".to_string(),
    };
    agent.prompt(command).await
}
