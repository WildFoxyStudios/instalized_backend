//! Background pin worker: drains `pin_jobs` with capped retries + linear backoff.
//! Without PINNING_API_URL the worker never starts (dev mode) — jobs stay pending.

use crate::state::AppState;

const MAX_ATTEMPTS: i64 = 5;
const BATCH: i64 = 10;

pub fn spawn(state: AppState) {
    if state.pinner.is_none() {
        tracing::info!("pinning: PINNING_API_URL not set — worker disabled");
        return;
    }
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(state.cfg.pin_worker_secs.max(1));
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = run_once(&state).await {
                tracing::warn!("pin worker pass failed: {e}");
            }
        }
    });
}

pub async fn run_once(state: &AppState) -> crate::error::AppResult<()> {
    let Some(pinner) = state.pinner.as_ref() else {
        return Ok(());
    };
    let jobs: Vec<(String, String, String, i64)> = state.db.read.with(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, cid, kind, attempts FROM pin_jobs
             WHERE status = 'pending' AND attempts < ?1
             ORDER BY created_at LIMIT ?2",
        )?;
        let rows = stmt
            .query_map([MAX_ATTEMPTS, BATCH], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;

    for (job_id, cid, kind, attempts) in jobs {
        let result = pinner.pin_by_cid(&cid, &format!("{kind}:{cid}")).await;
        let attempts = attempts + 1;
        state
            .db
            .writer
            .call(move |conn| {
                match result {
                    Ok(()) => {
                        conn.execute(
                            "UPDATE pin_jobs SET status='pinned', attempts=?2, pinned_at=?3 WHERE id=?1",
                            rusqlite::params![job_id, attempts, crate::db::now()],
                        )?;
                    }
                    Err(e) => {
                        tracing::warn!("pin {cid} attempt {attempts} failed: {e}");
                        let status = if attempts >= MAX_ATTEMPTS { "failed" } else { "pending" };
                        conn.execute(
                            "UPDATE pin_jobs SET attempts=?2, status=?3 WHERE id=?1",
                            rusqlite::params![job_id, attempts, status],
                        )?;
                    }
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}
