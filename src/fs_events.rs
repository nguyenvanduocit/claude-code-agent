//! Typed filesystem events from `FileChanged` / `CwdChanged` / `DirectoryAdded` hooks.
//!
//! Register a hook, then decode its payload:
//!
//! ```rust,no_run
//! use claude_code_agent::HookRegistry;
//! use claude_code_agent::fs_events::FsEvent;
//! use claude_code_agent::protocol::{HookEventKind, HookOutput};
//!
//! let mut hooks = HookRegistry::new();
//! hooks.on(HookEventKind::CwdChanged, None, |payload| async move {
//!     match FsEvent::from_payload(&payload) {
//!         Some(FsEvent::CwdChanged { new_cwd, .. }) => {
//!             // Watch the new project's sources; FileChanged hooks fire for them.
//!             HookOutput::watch_paths(HookEventKind::CwdChanged, vec![format!("{new_cwd}/src")])
//!         }
//!         _ => HookOutput::allow(),
//!     }
//! });
//! ```
//!
//! Limitation (verified against 2.1.328): the CLI delivers `CwdChanged` and
//! `FileChanged` to shell hooks declared in settings (`SessionOptions::settings`)
//! but not to SDK hook callbacks. Use `FsEvent::from_payload` on the JSON a
//! settings hook receives, or on payloads from a CLI version that forwards them.
//!
//! Source: binary hook input schemas (`FileChanged`: `file_path`, event;
//! `CwdChanged`: `old_cwd`, `new_cwd`; `DirectoryAdded`: directory)
use crate::protocol::{HookEventKind, HookEventPayload};

/// A filesystem hook event, decoded from its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsEvent {
    /// `event`: "change" | "add" | "unlink"
    FileChanged {
        /// The changed file (`file_path`).
        path: String,
        /// What happened to it.
        event: String,
    },
    /// The working directory changed.
    CwdChanged {
        /// Previous working directory.
        old_cwd: String,
        /// New working directory.
        new_cwd: String,
    },
    /// A directory was added (`DirectoryAdded`).
    DirectoryAdded {
        /// The added directory.
        directory: String,
    },
}

impl FsEvent {
    /// Decode a hook payload; `None` for other events or missing fields.
    #[must_use]
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
