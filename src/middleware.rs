/// Tower-style hook middleware stack.
///
/// Composable layers that wrap the HookRegistry.
/// Each layer receives the HookEventPayload and can:
///   - Short-circuit with HookOutput::block(...)
///   - Mutate and pass through
///   - Log, rate-limit, audit, etc.
use crate::protocol::{HookEventKind, HookOutput};

/// A no-op middleware placeholder.
/// Real implementations register async closures via HookRegistry.
pub struct MiddlewareStack;
pub struct LoggingMiddleware;
pub struct ToolAllowList {
    pub allowed: Vec<String>,
}

impl ToolAllowList {
    pub fn new(tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self { allowed: tools.into_iter().map(|s| s.into()).collect() }
    }
}

/// Middleware trait — register via HookRegistry::on().
pub trait HookMiddleware: Send + Sync {
    fn on_event(
        &self,
        payload: &crate::protocol::HookEventPayload,
    ) -> Option<HookOutput>;
}
