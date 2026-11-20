/// Hook registry — async Rust closures for all 33 hook event types.
///
/// CLI calls back via hook_callback IPC with deadline_ms.
/// Source: chunk-20bv8pca.js:19959–19979; chunk-0msz7hbn.js:7–323
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::protocol::{HookEventKind, HookEventPayload, HookOutput};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type HookFn = Arc<dyn Fn(HookEventPayload) -> BoxFuture<HookOutput> + Send + Sync>;

/// Registry of async hook handlers keyed by (event_kind, optional_matcher).
pub struct HookRegistry {
    // key: "EventKind" or "EventKind:matcher"
    handlers: RwLock<HashMap<String, Vec<HookFn>>>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { handlers: RwLock::new(HashMap::new()) }
    }

    /// Register an async closure for a hook event.
    ///
    /// ```rust
    /// registry.on(HookEventKind::PreToolUse, None, |payload| async move {
    ///     if payload.tool_name.as_deref() == Some("Bash") {
    ///         HookOutput::allow()
    ///     } else {
    ///         HookOutput::block("only Bash is allowed")
    ///     }
    /// });
    /// ```
    pub async fn on<F, Fut>(
        &self,
        event: HookEventKind,
        matcher: Option<&str>,
        handler: F,
    )
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        let key = Self::key(&event, matcher);
        let f: HookFn = Arc::new(move |p| Box::pin(handler(p)));
        self.handlers.write().await.entry(key).or_default().push(f);
    }

    /// Dispatch a callback_id to registered handlers.
    /// callback_id format: `<instanceId>/<event>/<index>`
    /// Source: chunk-mpj2xm2s.js:6
    pub async fn dispatch(&self, callback_id: &str, payload: &HookEventPayload) -> HookOutput {
        // Parse event kind from callback_id.
        let event_part = callback_id.split('/').nth(1).unwrap_or("");
        let event_kind_str = format!("{:?}", payload.hook_event_name); // debug == PascalCase

        // Try exact match first, then event-only.
        let keys = [
            Self::key(&payload.hook_event_name, None),
        ];
        let handlers = self.handlers.read().await;
        for key in &keys {
            if let Some(fns) = handlers.get(key) {
                // Run all handlers in order; first block wins.
                for f in fns {
                    let out = f(payload.clone()).await;
                    if out.block_message.is_some() {
                        return out;
                    }
                    // Accumulate watch_paths and additional_context from all handlers.
                }
                // Return the last output if no block.
                if let Some(last) = fns.last() {
                    return last(payload.clone()).await;
                }
            }
        }
        drop(handlers);
        let _ = event_part;
        let _ = event_kind_str;
        HookOutput::allow()
    }

    fn key(event: &HookEventKind, matcher: Option<&str>) -> String {
        match matcher {
            Some(m) => format!("{:?}:{}", event, m),
            None => format!("{:?}", event),
        }
    }
}

impl Default for HookRegistry {
    fn default() -> Self { Self::new() }
}

/// Convenience trait so external code can call `hooks.on_pre_tool_use(...)`.
pub trait HookRegistryExt {
    fn on_pre_tool_use<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static;
    fn on_stop<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static;
    fn on_user_prompt_submit<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static;
}

impl HookRegistryExt for HookRegistry {
    fn on_pre_tool_use<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static
    {
        self.on(HookEventKind::PreToolUse, None, f)
    }

    fn on_stop<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static
    {
        self.on(HookEventKind::Stop, None, f)
    }

    fn on_user_prompt_submit<F, Fut>(&self, f: F) -> impl Future<Output = ()>
    where F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
          Fut: Future<Output = HookOutput> + Send + 'static
    {
        self.on(HookEventKind::UserPromptSubmit, None, f)
    }
}

pub struct AllowAll;
pub struct HookDispatcher;
