/// Deterministic record/replay for testing.
///
/// Record: capture all InboundMessages to a JSONL file.
/// Replay: feed them back without spawning a real subprocess.
///
/// Source: Fork B design (record/replay via event stream)
use std::path::Path;

/// A recorded session — JSONL file of InboundMessages.
pub struct Recording {
    pub path: std::path::PathBuf,
}

impl Recording {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

/// A replay session that reads from a Recording instead of a live subprocess.
/// Implement by wrapping Session with a fake Child that reads from a file.
pub struct ReplaySession {
    pub recording: Recording,
}

impl ReplaySession {
    pub fn new(recording: Recording) -> Self {
        Self { recording }
    }
}
