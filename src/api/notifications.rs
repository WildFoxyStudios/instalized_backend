//! /v1/notifications + push token registry. `notify` is the shared helper that
//! persists and fans out over WS; offline users get it from REST on next open.

use crate::api::Page;
use crate::auth::AuthUser;
use crate::db::{new_id, now};
use crate::error::AppResult;
use crate::state::AppState;
use crate::ws::protocol;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

/// Persist + WS-push a notification. No-op when actor == recipient.
pub async fn notify(
    state: &AppState,
    user_id: &str,
    kind: &str,
    actor_id: &str,
    post_id: Option<&str>,
) {
    if user_id == actor_id {
        return;
    }
    let id = new_id();
    let created = now();
    let (uid, k, actor, pid) = (
        user_id.to_string(),
        kind.to_string(),
        actor_id.to_string(),
        post_id.map(String::from),
    );
    let insert = state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO notifications (id, user_id, kind, actor_id, post_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, uid, k, actor, pid, created],
            )?;
            Ok(())
        })
        .await;
    if let Err(e) = insert {
        tracing::warn!("notification insert failed: {e}");
        return;
    }
    let delivered = state
        .hub
        .send_to_user(
            user_id,
            &protocol::msg(
                "notif",
                json!({
                    "kind": kind, "actor_id": actor_id, "post_id": post_id, "created_at": created,
                }),
            ),
        )
        .await;
    if !delivered {
        tracing::debug!("user {user_id} offline — push notification would fire here");
    }
}

pub async fn list(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT n.id, n.kind, n.post_id, n.created_at, n.read_at,
                    a.id, a.username, a.display_name, a.avatar_cid
             FROM notifications n LEFT JOIN users a ON a.id = n.actor_id
             WHERE n.user_id = ?1
               AND (n.created_at < ?2 OR (n.created_at = ?2 AND n.id < ?3))
             ORDER BY n.created_at DESC, n.id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![me, cur_ts, cur_id, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "post_id": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, i64>(3)?,
                    "read_at": r.get::<_, Option<i64>>(4)?,
                    "actor": {
                        "id": r.get::<_, Option<String>>(5)?,
                        "username": r.get::<_, Option<String>>(6)?,
                        "display_name": r.get::<_, Option<String>>(7)?,
                        "avatar_cid": r.get::<_, Option<String>>(8)?,
                    },
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = crate::api::users::next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

#[derive(Deserialize)]
pub struct PushRegisterReq {
    pub platform: String,
    pub token: String,
}

pub async fn register_push(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<RegisterPushReq>,
) -> AppResult<Json<Value>> {
    let token = req.token;
    if token.len() > 4096 {
        return Err(AppError::bad_request("token too long"));
    }
    let me2 = me.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO push_tokens (user_id, platform, token, updated_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![me2, req.platform, token, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct UnregisterPushReq {
    pub token: String,
}

pub async fn unregister_push(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<UnregisterPushReq>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM push_tokens WHERE user_id = ?1 AND token = ?2",
                rusqlite::params![me, req.token],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}
