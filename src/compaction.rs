/// Semantic compaction via PreCompact hook.
///
/// When context approaches the limit, the CLI fires PreCompact.
/// This module intercepts that hook and injects a custom summary
/// produced by a separate summarizer session.
///
/// Source: chunk-20bv8pca.js hook list; compact_boundary schema line 15408
use crate::protocol::{HookEventKind, HookOutput};

/// A compaction handler that calls a user-supplied async summarizer.
/// Register with `session.hooks().on(HookEventKind::PreCompact, ...)`.
pub struct SemanticCompactor;

impl SemanticCompactor {
    /// Build a PreCompact hook closure that calls `summarizer` and injects
    /// its output as custom compact instructions.
    ///
    /// ```rust,no_run
    /// use claude_sdk::compaction::SemanticCompactor;
    /// use claude_sdk::protocol::{HookEventKind, HookOutput};
    ///
    /// session.hooks().on(HookEventKind::PreCompact, None, |payload| async move {
    ///     // Call your summarizer here
    ///     let summary = "Key context: ...".to_string();
    ///     HookOutput::compact_instructions(summary)
    /// }).await;
    /// ```
    pub fn new() -> Self { Self }
}
