//! Hook registry — async Rust closures for all 33 hook event types.
//!
//! Build it before spawning and pass it in `SessionOptions::hooks`: the CLI
//! learns which hooks exist only from the `initialize` request, so hooks
//! registered after spawn would never fire.
//!
//! Each `on()` call becomes one matcher with one callback id `hook_<n>`; when
//! the hook fires the CLI sends `hook_callback { callback_id: "hook_<n>" }`.
//! Source: claude_agent_sdk 0.1.48 _internal/query.py:128–147, :285–300
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::protocol::{HookEventKind, HookEventPayload, HookMatcherConfig, HookOutput};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type HookFn = Arc<dyn Fn(HookEventPayload) -> BoxFuture<HookOutput> + Send + Sync>;

#[derive(Clone)]
struct Entry {
    event: HookEventKind,
    matcher: Option<String>,
    timeout_s: Option<u32>,
    handler: HookFn,
}

/// Registry of async hook handlers. Cheap to clone.
#[derive(Clone, Default)]
pub struct HookRegistry {
    entries: Vec<Entry>,
}

impl fmt::Debug for HookRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.entries.iter().map(|e| (&e.event, &e.matcher)))
            .finish()
    }
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an async closure for a hook event.
    ///
    /// `matcher` filters by tool name for tool events (regex, e.g. `"Bash|Edit"`);
    /// `None` matches everything.
    ///
    /// ```rust,no_run
    /// # use claude_code_agent::HookRegistry;
    /// # use claude_code_agent::protocol::{HookEventKind, HookOutput};
    /// let mut hooks = HookRegistry::new();
    /// hooks.on(HookEventKind::PreToolUse, Some("Bash"), |payload| async move {
    ///     HookOutput::block("Bash is disabled")
    /// });
    /// ```
    pub fn on<F, Fut>(&mut self, event: HookEventKind, matcher: Option<&str>, handler: F) -> &mut Self
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        self.on_with_timeout(event, matcher, None, handler)
    }

    /// Same as `on`, with a per-hook timeout in seconds enforced by the CLI.
    pub fn on_with_timeout<F, Fut>(
        &mut self,
        event: HookEventKind,
        matcher: Option<&str>,
        timeout_s: Option<u32>,
        handler: F,
    ) -> &mut Self
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        let handler: HookFn = Arc::new(move |p| Box::pin(handler(p)));
        self.entries.push(Entry {
            event,
            matcher: matcher.map(str::to_owned),
            timeout_s,
            handler,
        });
        self
    }

    pub fn on_pre_tool_use<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        self.on(HookEventKind::PreToolUse, None, f)
    }

    pub fn on_stop<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        self.on(HookEventKind::Stop, None, f)
    }

    pub fn on_user_prompt_submit<F, Fut>(&mut self, f: F) -> &mut Self
    where
        F: Fn(HookEventPayload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = HookOutput> + Send + 'static,
    {
        self.on(HookEventKind::UserPromptSubmit, None, f)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The `hooks` field of the initialize request.
    pub fn initialize_config(&self) -> Option<HashMap<HookEventKind, Vec<HookMatcherConfig>>> {
        if self.entries.is_empty() {
            return None;
        }
        let mut config: HashMap<HookEventKind, Vec<HookMatcherConfig>> = HashMap::new();
        for (i, e) in self.entries.iter().enumerate() {
            config.entry(e.event.clone()).or_default().push(HookMatcherConfig {
                matcher: e.matcher.clone(),
                hook_callback_ids: vec![callback_id(i)],
                timeout: e.timeout_s,
            });
        }
        Some(config)
    }

    /// Run the handler registered under `callback_id`.
    /// Returns `None` when the id is unknown.
    pub async fn dispatch(&self, callback_id: &str, payload: HookEventPayload) -> Option<HookOutput> {
        let index: usize = callback_id.strip_prefix("hook_")?.parse().ok()?;
        let entry = self.entries.get(index)?;
        Some((entry.handler)(payload).await)
    }
}

fn callback_id(index: usize) -> String {
    format!("hook_{index}")
}
