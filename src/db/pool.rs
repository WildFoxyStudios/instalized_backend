//! Round-robin pool of read-only WAL connections. Queries here are sub-millisecond
//! at our scale; we accept brief blocking instead of paying spawn_blocking overhead.

use crate::error::{AppError, AppResult};
use rusqlite::{Connection, OpenFlags};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

pub struct ReadPool {
    conns: Vec<Mutex<Connection>>,
    next: AtomicUsize,
}

impl ReadPool {
    pub fn new(path: &str, n: usize) -> AppResult<Self> {
        let mut conns = Vec::with_capacity(n);
        for _ in 0..n.max(1) {
            let conn = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            conn.execute_batch("PRAGMA busy_timeout=5000; PRAGMA query_only=ON;")?;
            conns.push(Mutex::new(conn));
        }
        Ok(Self { conns, next: AtomicUsize::new(0) })
    }

    pub fn with<R>(&self, f: impl FnOnce(&Connection) -> AppResult<R>) -> AppResult<R> {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.conns.len();
        let conn = self
            .conns[i]
            .lock()
            .map_err(|_| AppError::internal("poisoned read connection"))?;
        f(&conn)
    }
}
