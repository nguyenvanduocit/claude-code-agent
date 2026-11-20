/// Session pool — N pre-warmed persistent processes.
///
/// Design from Fork C:
///   - Bounded concurrency via semaphore
///   - Idle session reuse — no spawn overhead on reuse
///   - Auto-respawn on process death
///   - PooledSession returns to pool on Drop
///
/// Source: Fork C internal architecture design; chunk-f89nsf5q.query.js
use std::sync::Arc;

use tokio::sync::{Mutex, Semaphore};

use crate::protocol::Initialize;
use crate::session::{Session, SessionOptions};

pub struct PoolConfig {
    pub max_sessions: usize,
    pub binary: String,
    pub code_mode: bool,
    pub tool_search: bool,
    pub include_partial: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_sessions: 4,
            binary: "claude".into(),
            code_mode: false,
            tool_search: false,
            include_partial: false,
        }
    }
}

struct PoolInner {
    idle: Mutex<Vec<Session>>,
    semaphore: Arc<Semaphore>,
    config: PoolConfig,
}

/// A pool of pre-warmed Claude sessions.
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

    /// Pre-warm N sessions in parallel.
    pub async fn warm(&self, n: usize, cwd: impl Into<String>) -> usize {
        let cwd = cwd.into();
        let mut tasks = Vec::with_capacity(n);
        for _ in 0..n {
            let pool = self.clone();
            let cwd = cwd.clone();
            tasks.push(tokio::spawn(async move {
                let opts = pool.0.config.make_opts();
                let init = Initialize::new(&cwd);
                Session::spawn(opts, init).await.ok()
            }));
        }
        let mut warmed = 0usize;
        for t in tasks {
            if let Ok(Some(s)) = t.await {
                self.0.idle.lock().await.push(s);
                warmed += 1;
            }
        }
        warmed
    }

    /// Acquire a session from the pool (blocks until one is available).
    pub async fn acquire(&self, cwd: impl Into<String>) -> PooledSession {
        let cwd = cwd.into();
        let permit = Arc::clone(&self.0.semaphore).acquire_owned().await.expect("semaphore closed");

        // Try to reuse an idle session.
        let session = {
            let mut idle = self.0.idle.lock().await;
            idle.pop().filter(|s| s.is_alive())
        };

        let session = match session {
            Some(s) => s,
            None => {
                // Spawn a new session.
                let opts = self.0.config.make_opts();
                let init = Initialize::new(&cwd);
                Session::spawn(opts, init).await.expect("spawn failed")
            }
        };

        PooledSession {
            session,
            pool: self.clone(),
            _permit: permit,
        }
    }
}

impl PoolConfig {
    fn make_opts(&self) -> SessionOptions {
        SessionOptions {
            binary: self.binary.clone().into(),
            code_mode: self.code_mode,
            tool_search: self.tool_search,
            include_partial: self.include_partial,
            ..Default::default()
        }
    }
}

/// A session borrowed from the pool. Returns automatically on Drop.
pub struct PooledSession {
    pub session: Session,
    pool: SessionPool,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl std::ops::Deref for PooledSession {
    type Target = Session;
    fn deref(&self) -> &Self::Target { &self.session }
}

impl Drop for PooledSession {
    fn drop(&mut self) {
        if self.session.is_alive() {
            let pool = self.pool.clone();
            let session = self.session.clone();
            tokio::spawn(async move {
                pool.0.idle.lock().await.push(session);
            });
        }
        // Permit dropped here → semaphore released.
    }
}
