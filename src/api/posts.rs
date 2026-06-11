//! /v1/posts/* — create (CID announcement), read, delete, likes, comments.
//! Like/comment counters go through the write-batcher (manifesto #6).

use crate::api::{post_from_row, validate_cid, POST_COLS};
use crate::auth::{AuthUser, MaybeUser};
use crate::db::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreatePostReq {
    pub kind: String,
    pub media_cid: String,
    pub thumb_cid: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub caption: Option<String>,
}

pub async fn create_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<CreatePostReq>,
) -> AppResult<Json<Value>> {
    if !["image", "video", "reel"].contains(&req.kind.as_str()) {
        return Err(AppError::bad_request("kind must be image|video|reel"));
    }
    validate_cid(&req.media_cid)?;
    if let Some(ref t) = req.thumb_cid {
        validate_cid(t)?;
    }
    let caption = req.caption.unwrap_or_default();
    if caption.len() > 2200 {
        return Err(AppError::bad_request("caption too long (max 2200)"));
    }
    let id = new_id();
    let id2 = id.clone();
    let me2 = me.clone();
    let media_cid = req.media_cid.clone();
    let thumb = req.thumb_cid.clone();
    // Parse tags + mentions from caption for the hashtag grid (cheap;
    // the regex is the same as the Flutter text_parser).
    let tags: Vec<String> = {
        let re = regex::Regex::new(r"#([A-Za-z0-9_]{1,140})").unwrap();
        re.captures_iter(&caption)
            .map(|c| c[1].to_lowercase())
            .collect()
    };
    let mentions: Vec<String> = {
        let re = regex::Regex::new(r"@([A-Za-z0-9._]{1,30})").unwrap();
        re.captures_iter(&caption)
            .map(|c| c[1].to_lowercase())
            .collect()
    };
    let tags2 = tags.clone();
    let mentions2 = mentions.clone();
    let id3 = id.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO posts (id, author_id, kind, media_cid, thumb_cid, width, height, duration_ms, caption, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id2, me2, req.kind, media_cid, thumb,
                    req.width, req.height, req.duration_ms, caption, now()
                ],
            )?;
            for tag in &tags2 {
                conn.execute(
                    "INSERT OR IGNORE INTO post_tags (post_id, tag) VALUES (?1, ?2)",
                    rusqlite::params![id3, tag],
                )?;
            }
            for username in &mentions2 {
                conn.execute(
                    "INSERT OR IGNORE INTO post_mentions (post_id, username) VALUES (?1, ?2)",
                    rusqlite::params![id3, username],
                )?;
            }
            Ok(())
        })
        .await?;
    // Durability floor: pin media (and thumb) — spec §10.
    crate::pinning::enqueue(&state.db, &req.media_cid, "media").await?;
    if let Some(ref t) = req.thumb_cid {
        crate::pinning::enqueue(&state.db, t, "thumb").await?;
    }
    get_post(State(state), MaybeUser(Some(me)), Path(id)).await
}

pub async fn get_post(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let me = me.unwrap_or_default();
    let item = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.id = ?2 AND p.deleted_at IS NULL"
        );
        Ok(conn.query_row(&sql, rusqlite::params![me, id], post_from_row)?)
    })?;
    Ok(Json(item))
}

pub async fn delete_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> AppResult<Json<Value>> {
    let hard = params.get("force").map(|v| v == "1").unwrap_or(false);
    let deleted = state
        .db
        .writer
        .call(move |conn| {
            if hard {
                let n = conn.execute(
                    "DELETE FROM posts WHERE id = ?1 AND author_id = ?2",
                    rusqlite::params![id, me],
                )?;
                Ok((n > 0, true))
            } else {
                let n = conn.execute(
                    "UPDATE posts SET deleted_at = ?1
                     WHERE id = ?2 AND author_id = ?3 AND deleted_at IS NULL",
                    rusqlite::params![now(), id, me],
                )?;
                Ok((n > 0, false))
            }
        })
        .await?;
    if !deleted.0 {
        return Err(AppError::not_found("post not found or not yours"));
    }
    Ok(Json(json!({ "deleted": true, "hard": deleted.1 })))
}

/// PUT like — idempotent; counter delta is batched, notification is immediate.
pub async fn like(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    let pid = post_id.clone();
    let (inserted, author) = state
        .db
        .writer
        .call(move |conn| {
            let author: String = conn
                .query_row(
                    "SELECT author_id FROM posts WHERE id = ?1 AND deleted_at IS NULL",
                    [&pid],
                    |r| r.get(0),
                )
                .map_err(|_| AppError::not_found("post not found"))?;
            let n = conn.execute(
                "INSERT OR IGNORE INTO likes (user_id, post_id, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![me2, pid, now()],
            )?;
            Ok((n > 0, author))
        })
        .await?;
    if inserted {
        state.db.writer.add_delta("posts", "like_count", &post_id, 1);
        crate::api::notifications::notify(&state, &author, "like", &me, Some(&post_id)).await;
    }
    Ok(Json(json!({ "liked": true })))
}

pub async fn unlike(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
) -> AppResult<Json<Value>> {
    let pid = post_id.clone();
    let removed = state
        .db
        .writer
        .call(move |conn| {
            let n = conn.execute(
                "DELETE FROM likes WHERE user_id = ?1 AND post_id = ?2",
                rusqlite::params![me, pid],
            )?;
            Ok(n > 0)
        })
        .await?;
    if removed {
        state.db.writer.add_delta("posts", "like_count", &post_id, -1);
    }
    Ok(Json(json!({ "liked": false })))
}

pub async fn comments_list(
    State(state): State<AppState>,
    MaybeUser(_me): MaybeUser,
    Path(post_id): Path<String>,
    Query(params): Query<CommentsPage>,
) -> AppResult<Json<Value>> {
    // Keyset cursor "created_at,id" or the sentinel "MAX,~" for the first page.
    let (cur_ts, cur_id) = match params
        .cursor
        .as_deref()
        .and_then(|c| c.split_once(','))
    {
        Some((ts, id)) => (ts.parse().unwrap_or(i64::MAX), id.to_string()),
        None => (i64::MAX, "~".to_string()),
    };
    let limit = params.limit.unwrap_or(20).clamp(1, 50);
    let parent_id = params.parent_id; // None = root comments
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = if parent_id.is_some() {
            conn.prepare(
                "SELECT c.id, c.body, c.created_at, u.id, u.username, u.display_name, u.avatar_cid
                 FROM comments c JOIN users u ON u.id = c.author_id
                 WHERE c.post_id = ?1 AND c.parent_id = ?5 AND c.deleted_at IS NULL
                   AND (c.created_at < ?2 OR (c.created_at = ?2 AND c.id < ?3))
                 ORDER BY c.created_at DESC, c.id DESC LIMIT ?4",
            )?
        } else {
            conn.prepare(
                "SELECT c.id, c.body, c.created_at, u.id, u.username, u.display_name, u.avatar_cid
                 FROM comments c JOIN users u ON u.id = c.author_id
                 WHERE c.post_id = ?1 AND c.parent_id IS NULL AND c.deleted_at IS NULL
                   AND (c.created_at < ?2 OR (c.created_at = ?2 AND c.id < ?3))
                 ORDER BY c.created_at DESC, c.id DESC LIMIT ?4",
            )?
        };
        let rows = if let Some(ref pid) = parent_id {
            stmt.query_map(rusqlite::params![post_id, cur_ts, cur_id, limit, pid], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "body": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, i64>(2)?,
                    "author": {
                        "id": r.get::<_, String>(3)?,
                        "username": r.get::<_, String>(4)?,
                        "display_name": r.get::<_, Option<String>>(5)?,
                        "avatar_cid": r.get::<_, Option<String>>(6)?,
                    },
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(rusqlite::params![post_id, cur_ts, cur_id, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "body": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, i64>(2)?,
                    "author": {
                        "id": r.get::<_, String>(3)?,
                        "username": r.get::<_, String>(4)?,
                        "display_name": r.get::<_, Option<String>>(5)?,
                        "avatar_cid": r.get::<_, Option<String>>(6)?,
                    },
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        Ok(rows)
    })?;
    let next_cursor = crate::api::users::next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

#[derive(Deserialize)]
pub struct CommentsPage {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub parent_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CommentReq {
    pub body: String,
    pub parent_id: Option<String>,
}

pub async fn comment_create(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(post_id): Path<String>,
    Json(req): Json<CommentReq>,
) -> AppResult<Json<Value>> {
    let body = req.body.trim().to_string();
    if body.is_empty() || body.len() > 2000 {
        return Err(AppError::bad_request("comment must be 1-2000 chars"));
    }
    let id = new_id();
    let created = now();
    let me2 = me.clone();
    let pid = post_id.clone();
    let id2 = id.clone();
    let body2 = body.clone();
    let parent = req.parent_id.clone();
    let parent_for_response = parent.clone();
    let author = state
        .db
        .writer
        .call(move |conn| {
            let author: String = conn
                .query_row(
                    "SELECT author_id FROM posts WHERE id = ?1 AND deleted_at IS NULL",
                    [&pid],
                    |r| r.get(0),
                )
                .map_err(|_| AppError::not_found("post not found"))?;
            // Verify parent comment exists and belongs to the same post.
            if let Some(ref p) = parent {
                let same_post: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM comments WHERE id = ?1 AND post_id = ?2 AND deleted_at IS NULL",
                        rusqlite::params![p, pid],
                        |r| r.get(0),
                    )?;
                if same_post == 0 {
                    return Err(AppError::bad_request("parent comment not found"));
                }
            }
            conn.execute(
                "INSERT INTO comments (id, post_id, author_id, body, parent_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id2, pid, me2, body2, parent, created],
            )?;
            Ok(author)
        })
        .await?;
    state
        .db
        .writer
        .add_delta("posts", "comment_count", &post_id, 1);
    crate::api::notifications::notify(&state, &author, "comment", &me, Some(&post_id)).await;
    Ok(Json(json!({
        "id": id, "post_id": post_id, "parent_id": parent_for_response, "body": body, "created_at": created,
    })))
}
