//! Parity v1.1: user search, explore grid, saved posts, follower/following
//! lists, comment deletion and content reports (the moderation inbox).

use crate::api::{post_from_row, users::next_cursor_from, Page, POST_COLS};
use crate::auth::{AuthUser, MaybeUser};
use crate::db::now;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
}

/// GET /v1/search/users?q= — username/display_name substring match.
pub async fn search_users(
    State(state): State<AppState>,
    AuthUser(_me): AuthUser,
    Query(query): Query<SearchQuery>,
) -> AppResult<Json<Value>> {
    let q = query.q.trim().to_lowercase();
    if q.len() < 2 {
        return Ok(Json(json!({ "items": [] })));
    }
    if q.len() > 64 {
        return Err(AppError::bad_request("query too long"));
    }
    // Escape LIKE wildcards in user input.
    let escaped = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let pattern = format!("%{escaped}%");
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, username, display_name, avatar_cid,
                    (SELECT COUNT(*) FROM follows WHERE followee_id = users.id) AS followers
             FROM users
             WHERE username LIKE ?1 ESCAPE '\\'
                OR LOWER(COALESCE(display_name,'')) LIKE ?1 ESCAPE '\\'
             ORDER BY followers DESC, username ASC
             LIMIT 20",
        )?;
        let rows = stmt
            .query_map([&pattern], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "username": r.get::<_, String>(1)?,
                    "display_name": r.get::<_, Option<String>>(2)?,
                    "avatar_cid": r.get::<_, Option<String>>(3)?,
                    "followers": r.get::<_, i64>(4)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

/// GET /v1/explore — recent posts from public accounts (keyset).
pub async fn explore(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let me = me.unwrap_or_default();
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             WHERE p.deleted_at IS NULL AND u.is_private = 0
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

/// PUT /v1/posts/{id}/save — bookmark (idempotent).
pub async fn save_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE id = ?1 AND deleted_at IS NULL",
                [&post_id],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(AppError::not_found("post not found"));
            }
            conn.execute(
                "INSERT OR IGNORE INTO saved_posts (user_id, post_id, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![me, post_id, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "saved": true })))
}

/// DELETE /v1/posts/{id}/save.
pub async fn unsave_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM saved_posts WHERE user_id = ?1 AND post_id = ?2",
                rusqlite::params![me, post_id],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "saved": false })))
}

/// GET /v1/me/saved — bookmarked posts, most recently saved first.
pub async fn saved_list(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let me2 = me.clone();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS}, s.created_at AS saved_at FROM saved_posts s
             JOIN posts p ON p.id = s.post_id AND p.deleted_at IS NULL
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             WHERE s.user_id = ?1
               AND (s.created_at < ?2 OR (s.created_at = ?2 AND s.post_id < ?3))
             ORDER BY s.created_at DESC, s.post_id DESC LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![me2, cur_ts, cur_id, limit], |r| {
                let mut v = post_from_row(r)?;
                v["saved_at"] = json!(r.get::<_, i64>(16)?);
                Ok(v)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    // Cursor over saved_at + post id.
    let next_cursor = if items.len() < limit as usize {
        None
    } else {
        items.last().map(|p| {
            format!("{},{}", p["saved_at"].as_i64().unwrap_or(0), p["id"].as_str().unwrap_or(""))
        })
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

fn follow_list(
    state: &AppState,
    username: &str,
    viewer: &str,
    page: &Page,
    followers_of: bool,
) -> AppResult<Value> {
    let uid = crate::api::users::user_id_by_username(state, username)?;
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let viewer = viewer.to_string();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        // followers_of: people whose follower_id rows point AT uid; else people uid follows.
        let sql = if followers_of {
            "SELECT u.id, u.username, u.display_name, u.avatar_cid, f.created_at,
                    (SELECT COUNT(*) FROM follows x WHERE x.follower_id = ?4 AND x.followee_id = u.id)
             FROM follows f JOIN users u ON u.id = f.follower_id
             WHERE f.followee_id = ?5
               AND (f.created_at < ?1 OR (f.created_at = ?1 AND u.id < ?2))
             ORDER BY f.created_at DESC, u.id DESC LIMIT ?3"
        } else {
            "SELECT u.id, u.username, u.display_name, u.avatar_cid, f.created_at,
                    (SELECT COUNT(*) FROM follows x WHERE x.follower_id = ?4 AND x.followee_id = u.id)
             FROM follows f JOIN users u ON u.id = f.followee_id
             WHERE f.follower_id = ?5
               AND (f.created_at < ?1 OR (f.created_at = ?1 AND u.id < ?2))
             ORDER BY f.created_at DESC, u.id DESC LIMIT ?3"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(rusqlite::params![cur_ts, cur_id, limit, viewer, uid], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "username": r.get::<_, String>(1)?,
                    "display_name": r.get::<_, Option<String>>(2)?,
                    "avatar_cid": r.get::<_, Option<String>>(3)?,
                    "followed_at": r.get::<_, i64>(4)?,
                    "is_following": r.get::<_, i64>(5)? > 0,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = if items.len() < limit as usize {
        None
    } else {
        items.last().map(|p| {
            format!("{},{}", p["followed_at"].as_i64().unwrap_or(0), p["id"].as_str().unwrap_or(""))
        })
    };
    Ok(json!({ "items": items, "next_cursor": next_cursor }))
}

/// GET /v1/users/{username}/followers
pub async fn followers(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Path(username): Path<String>,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    Ok(Json(follow_list(&state, &username, &me.unwrap_or_default(), &page, true)?))
}

/// GET /v1/users/{username}/following
pub async fn following(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Path(username): Path<String>,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    Ok(Json(follow_list(&state, &username, &me.unwrap_or_default(), &page, false)?))
}

/// DELETE /v1/comments/{id} — comment author OR post owner.
pub async fn delete_comment(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(comment_id): Path<String>,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    let post_id = state
        .db
        .writer
        .call(move |conn| {
            let (post_id, author_id): (String, String) = conn
                .query_row(
                    "SELECT post_id, author_id FROM comments WHERE id = ?1 AND deleted_at IS NULL",
                    [&comment_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| AppError::not_found("comment not found"))?;
            let post_owner: String = conn.query_row(
                "SELECT author_id FROM posts WHERE id = ?1",
                [&post_id],
                |r| r.get(0),
            )?;
            if me2 != author_id && me2 != post_owner {
                return Err(AppError::forbidden("not your comment"));
            }
            conn.execute(
                "UPDATE comments SET deleted_at = ?1 WHERE id = ?2",
                rusqlite::params![now(), comment_id],
            )?;
            Ok(post_id)
        })
        .await?;
    state.db.writer.add_delta("posts", "comment_count", &post_id, -1);
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize)]
pub struct ReportReq {
    #[serde(default)]
    pub reason: String,
}

/// POST /v1/posts/{id}/report — one report per user per post (moderation inbox).
pub async fn report_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
    Json(req): Json<ReportReq>,
) -> AppResult<Json<Value>> {
    let reason = req.reason.chars().take(500).collect::<String>();
    state
        .db
        .writer
        .call(move |conn| {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE id = ?1 AND deleted_at IS NULL",
                [&post_id],
                |r| r.get(0),
            )?;
            if exists == 0 {
                return Err(AppError::not_found("post not found"));
            }
            conn.execute(
                "INSERT OR IGNORE INTO reports (reporter_id, post_id, reason, created_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![me, post_id, reason, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "reported": true })))
}
