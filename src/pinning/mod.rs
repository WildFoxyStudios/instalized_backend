//! Retention layer (spec §10): pin-by-CID jobs against any IETF Pinning Service
//! API compatible provider (Pinata, Filebase, web3.storage).

pub mod client;
pub mod worker;

use crate::db::{new_id, now, Db};
use crate::error::AppResult;

/// Idempotently enqueue a pin job for a CID.
pub async fn enqueue(db: &Db, cid: &str, kind: &str) -> AppResult<()> {
    let cid = cid.to_string();
    let kind = kind.to_string();
    db.writer
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO pin_jobs (id, cid, kind, status, created_at)
                 VALUES (?1, ?2, ?3, 'pending', ?4)",
                rusqlite::params![new_id(), cid, kind, now()],
            )?;
            Ok(())
        })
        .await
}
