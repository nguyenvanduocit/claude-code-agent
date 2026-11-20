/// OpenTelemetry spans for turns and hooks.
///
/// Feature-gated: enable with `--features otel`.
/// Source: Fork C design; Fork A P9 implementation.

/// A turn span covering one prompt → result cycle.
/// Usage: drop when the Result message arrives.
pub struct TurnSpan {
    pub session_id: String,
    pub prompt: String,
}

impl TurnSpan {
    pub fn start(session_id: impl Into<String>, prompt: impl Into<String>) -> Self {
        Self { session_id: session_id.into(), prompt: prompt.into() }
    }

    /// Record cost and tokens when the turn completes.
    pub fn finish(&self, cost_usd: Option<f64>, input_tokens: Option<u64>, output_tokens: Option<u64>, ttft_ms: Option<u64>) {
        // TODO: emit OpenTelemetry span when otel feature is enabled
        let _ = (cost_usd, input_tokens, output_tokens, ttft_ms);
    }
}

/// A hook span covering one hook_callback execution.
pub struct HookSpan {
    pub callback_id: String,
    pub event: String,
    pub deadline_ms: u64,
}

impl HookSpan {
    pub fn start(callback_id: impl Into<String>, event: impl Into<String>, deadline_ms: u64) -> Self {
        Self { callback_id: callback_id.into(), event: event.into(), deadline_ms }
    }

    pub fn finish(&self, timed_out: bool) {
        let _ = timed_out;
    }
}
