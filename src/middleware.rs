/// Ready-made hook policies.
use crate::hooks::HookRegistry;
use crate::protocol::{HookEventKind, HookOutput};

/// Block every tool call whose tool is not in the list (PreToolUse hook).
///
/// ```rust,no_run
/// use claude_sdk::HookRegistry;
/// use claude_sdk::middleware::ToolAllowList;
///
/// let mut hooks = HookRegistry::new();
/// ToolAllowList::new(["Read", "Grep", "Glob"]).install(&mut hooks);
/// ```
#[derive(Debug, Clone)]
pub struct ToolAllowList {
    pub allowed: Vec<String>,
}

impl ToolAllowList {
    pub fn new(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { allowed: tools.into_iter().map(Into::into).collect() }
    }

    pub fn allows(&self, tool: &str) -> bool {
        self.allowed.iter().any(|t| t == tool)
    }

    /// Register the PreToolUse hook that enforces the list.
    pub fn install(self, hooks: &mut HookRegistry) {
        hooks.on(HookEventKind::PreToolUse, None, move |payload| {
            let verdict = match payload.tool_name.as_deref() {
                Some(tool) if !self.allows(tool) => Some(format!(
                    "Tool {tool} is not allowed here. Allowed tools: {}",
                    self.allowed.join(", ")
                )),
                _ => None,
            };
            async move { verdict.map(HookOutput::block).unwrap_or_else(HookOutput::allow) }
        });
    }
}
