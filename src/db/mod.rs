//! SQLite (WAL) access layer: single writer actor + read-only pool (spec §5).

pub mod pool;
pub mod writer;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use std::sync::Arc;

const MIGRATIONS: &[&str] = &[
    include_str!("../../migrations/001_init.sql"),
    include_str!("../../migrations/002_social.sql"),
    include_str!("../../migrations/003_social_v2.sql"),
    include_str!("../../migrations/004_totp.sql"),
    include_str!("../../migrations/005_push.sql"),
    include_str!("../../migrations/006_dm_media.sql"),
    include_str!("../../migrations/007_dm_reactions.sql"),
];

/// Unix epoch seconds.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs() as i64
}

pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

fn apply_pragmas(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA busy_timeout=5000;
         PRAGMA foreign_keys=ON;",
    )?;
    Ok(())
}

fn migrate(conn: &Connection) -> AppResult<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let target = (i + 1) as i64;
        if version < target {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", target)?;
            tracing::info!("applied migration {target}");
        }
    }
    Ok(())
}

/// Cloneable handle bundling the writer actor and the read pool.
#[derive(Clone)]
pub struct Db {
    pub writer: writer::Writer,
    pub read: Arc<pool::ReadPool>,
}

impl Db {
    /// Opens (creating if needed) the database, runs migrations, starts the writer actor.
    pub fn open(cfg: &Config) -> AppResult<Db> {
        if let Some(dir) = std::path::Path::new(&cfg.database_path).parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| AppError::internal(format!("create db dir: {e}")))?;
            }
        }
        let conn = Connection::open(&cfg.database_path)?;
        apply_pragmas(&conn)?;
        migrate(&conn)?;
        let writer = writer::Writer::start(conn, cfg.batch_flush_ms);
        let read = Arc::new(pool::ReadPool::new(&cfg.database_path, 4)?);
        Ok(Db { writer, read })
    }
}
