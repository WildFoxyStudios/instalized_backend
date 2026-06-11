//! /v1/feed (followed + self, keyset) and /v1/reels (global vertical video feed).

use crate::api::{post_from_row, users::next_cursor_from, Page, POST_COLS};
use crate::auth::AuthUser;
use crate::error::AppResult;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde_json::{json, Value};

pub async fn home_feed(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.deleted_at IS NULL
               AND (p.author_id = ?1 OR p.author_id IN
                    (SELECT followee_id FROM follows WHERE follower_id = ?1))
               AND (p.created_at < ?2 OR (p.created_at = ?2 AND p.id < ?3))
             ORDER BY p.created_at DESC, p.id DESC LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![me, cur_ts, cur_id, limit], post_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

pub async fn reels(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.deleted_at IS NULL AND p.kind = 'reel'
               AND u.is_private = 0
               AND (p.created_at < ?2 OR (p.created_at = ?2 AND p.id < ?3))
             ORDER BY p.created_at DESC, p.id DESC LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![me, cur_ts, cur_id, limit], post_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}
