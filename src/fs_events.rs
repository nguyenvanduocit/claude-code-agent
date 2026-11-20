/// Typed filesystem events from FileChanged / CwdChanged / DirectoryAdded hooks.
///
/// Register a hook, then decode its payload:
///
/// ```rust,no_run
/// use claude_sdk::HookRegistry;
/// use claude_sdk::fs_events::FsEvent;
/// use claude_sdk::protocol::{HookEventKind, HookOutput};
///
/// let mut hooks = HookRegistry::new();
/// hooks.on(HookEventKind::CwdChanged, None, |payload| async move {
///     match FsEvent::from_payload(&payload) {
///         Some(FsEvent::CwdChanged { new_cwd, .. }) => {
///             // Watch the new project's sources; FileChanged hooks fire for them.
///             HookOutput::watch_paths(HookEventKind::CwdChanged, vec![format!("{new_cwd}/src")])
///         }
///         _ => HookOutput::allow(),
///     }
/// });
/// ```
///
/// Limitation (verified against 2.1.328): the CLI delivers CwdChanged and
/// FileChanged to shell hooks declared in settings (`SessionOptions::settings`)
/// but not to SDK hook callbacks. Use `FsEvent::from_payload` on the JSON a
/// settings hook receives, or on payloads from a CLI version that forwards them.
///
/// Source: binary hook input schemas (FileChanged: file_path, event;
/// CwdChanged: old_cwd, new_cwd; DirectoryAdded: directory)
use crate::protocol::{HookEventKind, HookEventPayload};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FsEvent {
    /// `event`: "change" | "add" | "unlink"
    FileChanged { path: String, event: String },
    CwdChanged { old_cwd: String, new_cwd: String },
    DirectoryAdded { directory: String },
}

impl FsEvent {
    /// Decode a hook payload; `None` for other events or missing fields.
    pub fn from_payload(p: &HookEventPayload) -> Option<Self> {
        let extra = |k: &str| p.extra.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        match p.hook_event_name {
            HookEventKind::FileChanged => Some(Self::FileChanged {
                path: p.file_path.clone()?,
                event: p.event.clone()?,
            }),
            HookEventKind::CwdChanged => Some(Self::CwdChanged {
                old_cwd: extra("old_cwd")?,
                new_cwd: extra("new_cwd")?,
            }),
            HookEventKind::DirectoryAdded => Some(Self::DirectoryAdded { directory: extra("directory")? }),
            _ => None,
        }
    }
}
