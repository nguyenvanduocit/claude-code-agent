/// Session pool — pre-warmed persistent processes with idle reuse.
///
///   - at most `max_sessions` sessions are checked out at once (semaphore)
///   - every session uses `PoolConfig::options` (hooks, MCP servers, model, …)
///   - a returned session is reset with `/clear` before it is handed out
///     again, so one task never sees another task's conversation
///   - dead sessions are dropped and replaced by a fresh spawn
///   - a session returned while its turn is still running is not reused;
///     it ends once its last handle is dropped (until then it runs outside
///     the `max_sessions` count)
///   - model and permission mode changed with `set_model` /
///     `set_permission_mode` stay with the session across reuse
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::Semaphore;
use tokio_stream::StreamExt;

use crate::agent::{Agent, AgentEvent};
use crate::protocol::Initialize;
use crate::session::{Result, Session, SessionError, SessionOptions};

#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_sessions: usize,
    /// Options every pooled session is spawned with (`cwd`, hooks, …).
    pub options: SessionOptions,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self { max_sessions: 4, options: SessionOptions::default() }
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
        let Ok(_permits) = Arc::clone(&self.0.semaphore).try_acquire_many_owned(count as u32) else {
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

    /// Check out a session with an empty conversation; waits while
    /// `max_sessions` are checked out.
    pub async fn acquire(&self) -> Result<PooledSession> {
        let permit = Arc::clone(&self.0.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| SessionError::Closed)?;

        loop {
            let next = self.idle().pop();
            let Some(Idle { session, used }) = next else { break };
            if !session.is_alive() {
                continue;
            }
            if used && clear(&session).await.is_err() {
                session.close().await;
                continue;
            }
            return Ok(PooledSession { session, pool: self.clone(), _permit: permit });
        }

        let session = Session::spawn(self.0.config.options.clone(), Initialize::default()).await?;
        Ok(PooledSession { session, pool: self.clone(), _permit: permit })
    }

    fn idle(&self) -> MutexGuard<'_, Vec<Idle>> {
        self.0.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `/clear` makes no API call; anything slower means a stuck CLI.
const CLEAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Reset the conversation: `/clear` runs locally in the CLI (no API call)
/// and starts a new session id.
async fn clear(session: &Session) -> std::result::Result<(), String> {
    let run = async {
        let mut stream = Agent::new(session.clone()).prompt("/clear").await.map_err(|e| e.to_string())?;
        while let Some(ev) = stream.next().await {
            match ev {
                AgentEvent::Done(_) => return Ok(()),
                AgentEvent::Error(e) => return Err(e),
                _ => {}
            }
        }
        Err("stream ended without a result".to_string())
    };
    tokio::time::timeout(CLEAR_TIMEOUT, run).await.unwrap_or_else(|_| Err("/clear timed out".into()))
}

/// A session borrowed from the pool. Returns to the pool on Drop. Keep
/// clones of `session` (e.g. inside an `Agent`) no longer than the
/// `PooledSession`: after Drop another task may get the same session.
pub struct PooledSession {
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
