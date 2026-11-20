/// Reactive filesystem events via FileChanged/CwdChanged hooks.
///
/// Source: chunk-0msz7hbn.js:288–305 (FileChanged, CwdChanged, DirectoryAdded)
/// hookSpecificOutput.watchPaths[] registers paths to watch.
use crate::protocol::{HookEventKind, HookOutput};

#[derive(Debug, Clone)]
pub enum FsEvent {
    FileChanged { path: String, event: String },
    CwdChanged { cwd: String },
    DirectoryAdded { directory: String },
}

/// Helper for registering filesystem-reactive hooks.
///
/// ```rust,no_run
/// session.hooks().on(HookEventKind::CwdChanged, None, |payload| async move {
///     HookOutput {
///         watch_paths: vec![
///             format!("{}/src/**/*.rs", payload.cwd.unwrap_or_default()),
///         ],
///         ..HookOutput::allow()
///     }
/// }).await;
/// ```
pub struct FsEventHandler;
