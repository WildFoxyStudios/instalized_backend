//! Single-writer actor (WAL has exactly one writer) + in-memory counter batching
//! (manifesto #6): hot counters accumulate and flush in one transaction every
//! `flush_ms` or every 500 deltas, whichever comes first.

use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use std::collections::HashMap;
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

enum Msg {
    Job(Job),
    /// (table, column, row id, delta) — table/column are compile-time constants.
    Delta(&'static str, &'static str, String, i64),
    Flush(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct Writer {
    tx: mpsc::UnboundedSender<Msg>,
}

const MAX_PENDING_DELTAS: usize = 500;

impl Writer {
    pub fn start(mut conn: Connection, flush_ms: u64) -> Writer {
        let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
        tokio::spawn(async move {
            let mut deltas: HashMap<(&'static str, &'static str, String), i64> = HashMap::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(flush_ms.max(10)));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some(Msg::Job(job)) => job(&mut conn),
                            Some(Msg::Delta(t, c, id, d)) => {
                                *deltas.entry((t, c, id)).or_insert(0) += d;
                                if deltas.len() >= MAX_PENDING_DELTAS {
                                    flush(&mut conn, &mut deltas);
                                }
                            }
                            Some(Msg::Flush(ack)) => {
                                flush(&mut conn, &mut deltas);
                                let _ = ack.send(());
                            }
                            None => {
                                flush(&mut conn, &mut deltas);
                                break;
                            }
                        }
                    }
                    _ = tick.tick() => flush(&mut conn, &mut deltas),
                }
            }
        });
        Writer { tx }
    }

    /// Run a write closure on the writer connection and await its result.
    pub async fn call<R, F>(&self, f: F) -> AppResult<R>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> AppResult<R> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Job(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            })))
            .map_err(|_| AppError::internal("writer actor gone"))?;
        rx.await.map_err(|_| AppError::internal("writer dropped reply"))?
    }

    /// Queue a counter delta; durable on next flush (≤ flush_ms later).
    pub fn add_delta(&self, table: &'static str, column: &'static str, id: &str, delta: i64) {
        let _ = self.tx.send(Msg::Delta(table, column, id.to_string(), delta));
    }

    /// Force a flush and wait for it (used by tests and graceful shutdown).
    pub async fn flush_now(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

fn flush(conn: &mut Connection, deltas: &mut HashMap<(&'static str, &'static str, String), i64>) {
    if deltas.is_empty() {
        return;
    }
    let result = (|| -> rusqlite::Result<()> {
        let tx = conn.transaction()?;
        for ((table, column, id), d) in deltas.iter() {
            if *d != 0 {
                // table/column are &'static str constants from call sites — not user input.
                let sql = format!("UPDATE {table} SET {column} = MAX(0, {column} + ?1) WHERE id = ?2");
                tx.execute(&sql, rusqlite::params![d, id])?;
            }
        }
        tx.commit()
    })();
    if let Err(e) = result {
        tracing::error!("counter flush failed (deltas dropped): {e}");
    }
    deltas.clear();
}
