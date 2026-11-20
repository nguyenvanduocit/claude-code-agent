//! Session pool — pre-warmed persistent processes with idle reuse.
//!
//!   - at most `max_sessions` sessions are checked out at once (semaphore)
//!   - every session uses `PoolConfig::options` (hooks, MCP servers, model, …)
//!   - `acquire_with(Checkout { model, effort, permission_mode })` hands out
//!     a session with its own model, effort or permission mode; `None`
//!     keeps the pool's value
//!   - a returned session is reset before it is handed out again: `/clear`
//!     empties the conversation, and model, effort and permission mode are
//!     set for the next checkout, so a task may change them for itself
//!     (`set_model`, `set_effort`, `set_permission_mode`) without affecting
//!     the next one
//!   - dead sessions are dropped and replaced by a fresh spawn
//!   - a session returned while its turn is still running is not reused;
//!     it ends once its last handle is dropped (until then it runs outside
//!     the `max_sessions` count)
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

use crate::agent::{Agent, AgentEvent};
use crate::protocol::{Effort, Initialize, PermissionMode};
use crate::session::{Result, Session, SessionError, SessionOptions};

/// Configuration of a `SessionPool`.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Most sessions checked out at once. Default 4.
    pub max_sessions: usize,
    /// Options every pooled session is spawned with (`cwd`, hooks, …).
    pub options: SessionOptions,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_sessions: 4, options: SessionOptions::default() }
    }
}

/// Settings for one checkout. `None` keeps the value from
/// `PoolConfig::options`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checkout {
    /// Model (`Session::set_model`).
    pub model: Option<String>,
    /// Effort level (`Session::set_effort`).
    pub effort: Option<Effort>,
    /// Permission mode (`Session::set_permission_mode`).
    pub permission_mode: Option<PermissionMode>,
}

impl Checkout {
    /// The pool's options with this checkout's settings applied.
    fn options(&self, base: &SessionOptions) -> SessionOptions {
        SessionOptions {
            model: self.model.clone().or_else(|| base.model.clone()),
            effort: self.effort.or(base.effort),
            permission_mode: self.permission_mode.or(base.permission_mode),
            ..base.clone()
        }
    }
}

struct Idle {
    session: Session,
    /// Holds a previous task's conversation; needs `/clear` before reuse.
    used: bool,
}

struct PoolInner {
    idle: Mutex<Vec<Idle>>,
    semaphore: Arc<Semaphore>,
    config: PoolConfig,
}

/// A pool of pre-warmed Claude sessions. Cheap to clone.
#[derive(Clone)]
pub struct SessionPool(Arc<PoolInner>);

impl SessionPool {
    /// An empty pool; sessions spawn on `warm` or `acquire`.
    #[must_use]
    pub fn new(config: PoolConfig) -> Self {
        let max = config.max_sessions;
        SessionPool(Arc::new(PoolInner {
            idle: Mutex::new(Vec::with_capacity(max)),
            semaphore: Arc::new(Semaphore::new(max)),
            config,
        }))
    }

    /// Spawn up to `n` idle sessions in parallel, within `max_sessions`
    /// idle plus checked-out sessions. Returns how many were added.
    pub async fn warm(&self, n: usize) -> usize {
        let room = self.0.semaphore.available_permits().saturating_sub(self.idle().len());
        let count = n.min(room);
        // Held while spawning, so a concurrent acquire() cannot spawn past the limit.
        let Ok(permits) = u32::try_from(count) else { return 0 };
        let Ok(_permits) = Arc::clone(&self.0.semaphore).try_acquire_many_owned(permits) else {
            return 0;
        };
        let tasks: Vec<_> = (0..count)
            .map(|_| {
                let opts = self.0.config.options.clone();
                tokio::spawn(async move { Session::spawn(opts, Initialize::default()).await })
            })
            .collect();
        let mut warmed = 0;
        for t in tasks {
            match t.await {
                Ok(Ok(session)) => {
                    self.idle().push(Idle { session, used: false });
                    warmed += 1;
                }
                Ok(Err(e)) => tracing::warn!(error = %e, "pool warm-up spawn failed"),
                Err(e) => tracing::warn!(error = %e, "pool warm-up task failed"),
            }
        }
        warmed
    }

    /// Check out a session with an empty conversation and the pool's
    /// settings; waits while `max_sessions` are checked out.
    pub async fn acquire(&self) -> Result<PooledSession> {
        self.acquire_with(Checkout::default()).await
    }

    /// Check out a session with an empty conversation and `checkout`'s
    /// model, effort and permission mode; waits while `max_sessions` are
    /// checked out.
    pub async fn acquire_with(&self, checkout: Checkout) -> Result<PooledSession> {
        let permit = Arc::clone(&self.0.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| SessionError::Closed)?;
        let options = checkout.options(&self.0.config.options);

        loop {
            let next = self.idle().pop();
            let Some(Idle { session, used }) = next else { break };
            if !session.is_alive() {
                continue;
            }
            // An unused session already runs the pool's settings.
            let prepared = match (used, checkout == Checkout::default()) {
                (false, true) => Ok(()),
                (false, false) => apply(&session, &options).await,
                (true, _) => reset(&session, &options).await,
            };
            if let Err(e) = prepared {
                tracing::warn!(error = %e, "pooled session reset failed; replacing it");
                session.close().await;
                continue;
            }
            return Ok(PooledSession { session, pool: self.clone(), _permit: permit });
        }

        let session = Session::spawn(options, Initialize::default()).await?;
        Ok(PooledSession { session, pool: self.clone(), _permit: permit })
    }

    fn idle(&self) -> MutexGuard<'_, Vec<Idle>> {
        self.0.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A reset makes no API call; anything slower means a stuck CLI.
const RESET_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reset a used session for the next task: `/clear` the conversation (runs
/// locally in the CLI, no API call, new session id), then set the next
/// checkout's model, effort and permission mode.
async fn reset(session: &Session, options: &SessionOptions) -> std::result::Result<(), String> {
    let run = async {
        let mut stream = Agent::new(session.clone()).prompt("/clear").await.map_err(|e| e.to_string())?;
        loop {
            match stream.next().await {
                Some(AgentEvent::Done(_)) => break,
                Some(AgentEvent::Error(e)) => return Err(e.to_string()),
                Some(_) => {}
                None => return Err("stream ended without a result".to_string()),
            }
        }
        apply(session, options).await
    };
    tokio::time::timeout(RESET_TIMEOUT, run).await.unwrap_or_else(|_| Err("reset timed out".into()))
}

/// Set model, effort and permission mode from `options` on a live session.
async fn apply(session: &Session, options: &SessionOptions) -> std::result::Result<(), String> {
    session.set_model(options.model.as_deref()).await.map_err(|e| e.to_string())?;
    session.set_effort(options.effort).await.map_err(|e| e.to_string())?;
    let mode = options.permission_mode.unwrap_or(PermissionMode::Default);
    session.set_permission_mode(mode).await.map_err(|e| e.to_string())
}

/// A session borrowed from the pool. Returns to the pool on Drop. Keep
/// clones of `session` (e.g. inside an `Agent`) no longer than the
/// `PooledSession`: after Drop another task may get the same session.
pub struct PooledSession {
    /// The borrowed session.
    pub session: Session,
    pool: SessionPool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl std::ops::Deref for PooledSession {
    type Target = Session;
    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl Drop for PooledSession {
    fn drop(&mut self) {
        // Synchronous: safe to drop outside a Tokio runtime.
        if self.session.is_alive() && !self.session.is_busy() {
            self.pool.idle().push(Idle { session: self.session.clone(), used: true });
        }
        // Permit dropped here → semaphore released.
    }
}
