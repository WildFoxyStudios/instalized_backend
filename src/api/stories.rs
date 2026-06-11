//! /v1/stories/* — 24 h TTL stories. The sweeper hard-deletes expired rows
//! (manifesto: auto-elimination in the DB; P2P copies fade as caches evict).

use crate::auth::AuthUser;
use crate::db::{new_id, now};
use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

pub const STORY_TTL_SECS: i64 = 24 * 3600;

#[derive(Deserialize)]
pub struct CreateStoryReq {
    pub media_cid: String,
    pub thumb_cid: Option<String>,
    /// Tests override the TTL to exercise the sweeper; clients omit it.
    pub ttl_secs: Option<i64>,
}

pub async fn create_story(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<CreateStoryReq>,
) -> AppResult<Json<Value>> {
    crate::api::validate_cid(&req.media_cid)?;
    let ttl = req.ttl_secs.unwrap_or(STORY_TTL_SECS).clamp(1, STORY_TTL_SECS);
    let id = new_id();
    let created = now();
    let expires = created + ttl;
    let id2 = id.clone();
    let media = req.media_cid.clone();
    let thumb = req.thumb_cid.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO stories (id, author_id, media_cid, thumb_cid, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id2, me, media, thumb, created, expires],
            )?;
            Ok(())
        })
        .await?;
    crate::pinning::enqueue(&state.db, &req.media_cid, "story").await?;
    Ok(Json(json!({
        "id": id, "media_cid": req.media_cid, "thumb_cid": req.thumb_cid,
        "created_at": created, "expires_at": expires,
    })))
}

/// Active stories from followed users + self, grouped by author client-side.
pub async fn stories_feed(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(_q): Query<serde_json::Map<String, Value>>,
) -> AppResult<Json<Value>> {
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.media_cid, s.thumb_cid, s.created_at, s.expires_at,
                    u.id, u.username, u.display_name, u.avatar_cid
             FROM stories s JOIN users u ON u.id = s.author_id
             WHERE s.expires_at > ?2
               AND (s.author_id = ?1 OR s.author_id IN
                    (SELECT followee_id FROM follows WHERE follower_id = ?1))
             ORDER BY u.id, s.created_at ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![me, now()], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "media_cid": r.get::<_, String>(1)?,
                    "thumb_cid": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, i64>(3)?,
                    "expires_at": r.get::<_, i64>(4)?,
                    "author": {
                        "id": r.get::<_, String>(5)?,
                        "username": r.get::<_, String>(6)?,
                        "display_name": r.get::<_, Option<String>>(7)?,
                        "avatar_cid": r.get::<_, Option<String>>(8)?,
                    },
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

/// Background sweeper — hard-deletes expired stories every `story_sweep_secs`.
pub fn spawn_sweeper(state: AppState) {
    tokio::spawn(async move {
        let period = std::time::Duration::from_secs(state.cfg.story_sweep_secs.max(1));
        let mut tick = tokio::time::interval(period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let result = state
                .db
                .writer
                .call(|conn| {
                    let n = conn.execute("DELETE FROM stories WHERE expires_at < ?1", [now()])?;
                    Ok(n)
                })
                .await;
            match result {
                Ok(n) if n > 0 => tracing::info!("story sweeper: deleted {n} expired"),
                Ok(_) => {}
                Err(e) => tracing::warn!("story sweeper failed: {e}"),
            }
        }
    });
}
