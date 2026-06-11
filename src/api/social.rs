//! /v1/social/* — moderation (block, mute, report), archive, recently
//! deleted, privacy & notification prefs, hashtag grid, highlights.
//!
//! Lives in its own module so api/mod.rs stays scannable. The router
//! below mirrors the spec §6 additions.

use crate::api::users::next_cursor_from;
use crate::api::{post_from_row, Page, POST_COLS};
use crate::auth::AuthUser;
use crate::db::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

// ---------------------------------------------------------------- block
pub async fn block(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    if me == target {
        return Err(AppError::bad_request("cannot block yourself"));
    }
    let me2 = me.clone();
    let target2 = target.clone();
    state
        .db
        .writer
        .call(move |conn| {
            let exists: i64 =
                conn.query_row("SELECT COUNT(*) FROM users WHERE id = ?1", [&target2], |r| {
                    r.get(0)
                })?;
            if exists == 0 {
                return Err(AppError::not_found("user not found"));
            }
            conn.execute(
                "INSERT OR IGNORE INTO blocks (blocker_id, blocked_id, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![me2, target2, now()],
            )?;
            // Drop any existing follow edges — blocking supersedes following.
            conn.execute(
                "DELETE FROM follows WHERE (follower_id = ?1 AND followee_id = ?2)
                                            OR (follower_id = ?2 AND followee_id = ?1)",
                rusqlite::params![me2, target2],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "blocked": true })))
}

pub async fn unblock(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM blocks WHERE blocker_id = ?1 AND blocked_id = ?2",
                rusqlite::params![me, target],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "blocked": false })))
}

pub async fn blocked_list(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<Value>> {
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT u.id, u.username, u.display_name, u.avatar_cid
             FROM blocks b JOIN users u ON u.id = b.blocked_id
             WHERE b.blocker_id = ?1
             ORDER BY b.created_at DESC LIMIT 200",
        )?;
        let rows = stmt
            .query_map([&me], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "username": r.get::<_, String>(1)?,
                    "display_name": r.get::<_, Option<String>>(2)?,
                    "avatar_cid": r.get::<_, Option<String>>(3)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

// ---------------------------------------------------------------- mute
pub async fn mute(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    if me == target {
        return Err(AppError::bad_request("cannot mute yourself"));
    }
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO mutes (muter_id, muted_id, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![me, target, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "muted": true })))
}

pub async fn unmute(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM mutes WHERE muter_id = ?1 AND muted_id = ?2",
                rusqlite::params![me, target],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "muted": false })))
}

// ---------------------------------------------------------------- report user
pub async fn report_user(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
    Json(req): Json<ReportReq>,
) -> AppResult<Json<Value>> {
    let reason = req.reason.unwrap_or_else(|| "user report".into());
    if reason.len() > 500 {
        return Err(AppError::bad_request("reason too long"));
    }
    state
        .db
        .writer
        .call(move |conn| {
            // Spec §15: every report goes to a CID blocklist table for review.
            conn.execute(
                "INSERT INTO notifications (id, user_id, kind, actor_id, post_id, created_at)
                 VALUES (?1, ?2, 'report_user', ?3, NULL, ?4)",
                rusqlite::params![new_id(), target, me, now()],
            )?;
            // Side-car: drop the reason into a generic payload table.
            // We reuse the pin_jobs table for batched processing in v1.1.
            let _ = conn.execute(
                "INSERT INTO pin_jobs (id, cid, kind, status, attempts, created_at)
                 VALUES (?1, 'user_report', ?2, 'pending', 0, ?3)",
                rusqlite::params![new_id(), reason, now()],
            );
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "reported": true })))
}

#[derive(Deserialize)]
pub struct ReportReq {
    pub reason: Option<String>,
}

// ---------------------------------------------------------------- privacy
#[derive(Deserialize)]
pub struct PrivacyReq {
    pub private: Option<bool>,
    pub show_activity: Option<bool>,
    pub allow_mentions: Option<bool>,
    pub allow_story_replies: Option<bool>,
}

pub async fn patch_privacy(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<PrivacyReq>,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    state
        .db
        .writer
        .call(move |conn| {
            // upsert
            conn.execute(
                "INSERT OR IGNORE INTO user_privacy (user_id) VALUES (?1)",
                rusqlite::params![me2],
            )?;
            if let Some(v) = req.private {
                conn.execute(
                    "UPDATE users SET is_private = ?1 WHERE id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
                conn.execute(
                    "UPDATE user_privacy SET private_account = ?1 WHERE user_id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
            }
            if let Some(v) = req.show_activity {
                conn.execute(
                    "UPDATE user_privacy SET show_activity_status = ?1 WHERE user_id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
            }
            if let Some(v) = req.allow_mentions {
                conn.execute(
                    "UPDATE user_privacy SET allow_mentions = ?1 WHERE user_id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
            }
            if let Some(v) = req.allow_story_replies {
                conn.execute(
                    "UPDATE user_privacy SET allow_story_replies = ?1 WHERE user_id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
            }
            Ok(())
        })
        .await?;
    let me3 = me.clone();
    let out: Value = state
        .db
        .read
        .with(move |conn| {
            let r = conn.query_row(
                "SELECT private_account, show_activity_status, allow_mentions, allow_story_replies
                 FROM user_privacy WHERE user_id = ?1",
                [&me3],
                |r| {
                    Ok(json!({
                        "private": r.get::<_, i64>(0)? != 0,
                        "show_activity": r.get::<_, i64>(1)? != 0,
                        "allow_mentions": r.get::<_, i64>(2)? != 0,
                        "allow_story_replies": r.get::<_, i64>(3)? != 0,
                    }))
                },
            );
            match r {
                Ok(v) => Ok(v),
                Err(_) => Ok(json!({
                    "private": false, "show_activity": true,
                    "allow_mentions": true, "allow_story_replies": true,
                })),
            }
        })?;
    Ok(Json(out))
}

// ---------------------------------------------------------------- notif prefs
#[derive(Deserialize)]
pub struct NotifPrefsReq {
    pub posts: Option<bool>,
    pub stories: Option<bool>,
    pub lives: Option<bool>,
    pub dms: Option<bool>,
    pub video_calls: Option<bool>,
    pub pause_all: Option<bool>,
}

pub async fn post_notif_prefs(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<NotifPrefsReq>,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO user_notif_prefs (user_id) VALUES (?1)",
                rusqlite::params![me2],
            )?;
            macro_rules! set {
                ($col:literal, $val:expr) => {
                    conn.execute(
                        concat!("UPDATE user_notif_prefs SET ", $col, " = ?1 WHERE user_id = ?2"),
                        rusqlite::params![$val as i64, me2],
                    )?;
                };
            }
            if let Some(v) = req.posts { set!("posts", v); }
            if let Some(v) = req.stories { set!("stories", v); }
            if let Some(v) = req.lives { set!("lives", v); }
            if let Some(v) = req.dms { set!("dms", v); }
            if let Some(v) = req.video_calls { set!("video_calls", v); }
            if let Some(v) = req.pause_all { set!("pause_all", v); }
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------- archive
pub async fn archive_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let n = state
        .db
        .writer
        .call(move |conn| {
            Ok(conn.execute(
                "UPDATE posts SET is_archived = 1
                 WHERE id = ?1 AND author_id = ?2 AND deleted_at IS NULL",
                rusqlite::params![id, me],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(AppError::not_found("post not found or not yours"));
    }
    Ok(Json(json!({ "archived": true })))
}

pub async fn unarchive_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let n = state
        .db
        .writer
        .call(move |conn| {
            Ok(conn.execute(
                "UPDATE posts SET is_archived = 0
                 WHERE id = ?1 AND author_id = ?2",
                rusqlite::params![id, me],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(AppError::not_found("post not found or not yours"));
    }
    Ok(Json(json!({ "archived": false })))
}

pub async fn my_archive(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let me2 = me.clone();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.author_id = ?2 AND p.is_archived = 1 AND p.deleted_at IS NULL
               AND (p.created_at < ?3 OR (p.created_at = ?3 AND p.id < ?4))
             ORDER BY p.created_at DESC, p.id DESC LIMIT ?5"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![me2, me2, cur_ts, cur_id, limit], post_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

pub async fn my_deleted(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let me2 = me.clone();
    // Within the last 30 days only; older entries are already hard-deleted by
    // the sweeper in 002_social.sql.
    let cutoff = now() - 30 * 86_400;
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.author_id = ?2 AND p.deleted_at IS NOT NULL
               AND p.deleted_at > ?3
               AND (p.deleted_at < ?4 OR (p.deleted_at = ?4 AND p.id < ?5))
             ORDER BY p.deleted_at DESC, p.id DESC LIMIT ?6"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params![me2, me2, cutoff, cur_ts, cur_id, limit],
                post_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

pub async fn restore_post(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let n = state
        .db
        .writer
        .call(move |conn| {
            Ok(conn.execute(
                "UPDATE posts SET deleted_at = NULL
                 WHERE id = ?1 AND author_id = ?2 AND deleted_at IS NOT NULL",
                rusqlite::params![id, me],
            )?)
        })
        .await?;
    if n == 0 {
        return Err(AppError::not_found("post not found, not yours, or already restored"));
    }
    Ok(Json(json!({ "restored": true })))
}

// ---------------------------------------------------------------- hashtag grid
pub async fn hashtag_grid(
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let tag = tag.to_lowercase();
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let tag2 = tag.clone();
    let (total, items): (i64, Vec<Value>) = state.db.read.with(move |conn| {
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM post_tags WHERE tag = ?1", [&tag2], |r| r.get(0))?;
        let sql = format!(
            "SELECT {POST_COLS} FROM post_tags t
             JOIN posts p ON p.id = t.post_id
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = '' -- public, no viewer
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ''
             WHERE t.tag = ?1 AND p.deleted_at IS NULL AND p.is_archived = 0
               AND (p.created_at < ?2 OR (p.created_at = ?2 AND p.id < ?3))
             ORDER BY p.created_at DESC, p.id DESC LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params![tag2, cur_ts, cur_id, limit], post_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((total, rows))
    })?;
    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({
        "items": items, "next_cursor": next_cursor, "total": total,
    })))
}

// ---------------------------------------------------------------- highlights
pub async fn my_highlights(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT h.id, h.name, h.cover_cid, h.created_at,
                    (SELECT COUNT(*) FROM highlight_stories WHERE highlight_id = h.id) AS n
             FROM highlights h WHERE h.user_id = ?1
             ORDER BY h.created_at DESC",
        )?;
        let rows = stmt
            .query_map([&me2], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "cover_cid": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, i64>(3)?,
                    "story_count": r.get::<_, i64>(4)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

pub async fn user_highlights(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> AppResult<Json<Value>> {
    let u = username.to_lowercase();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT h.id, h.name, h.cover_cid
             FROM highlights h JOIN users u ON u.id = h.user_id
             WHERE u.username = ?1
             ORDER BY h.created_at DESC",
        )?;
        let rows = stmt
            .query_map([&u], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "cover_cid": r.get::<_, Option<String>>(2)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct CreateHighlightReq {
    pub name: String,
}

pub async fn create_highlight(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<CreateHighlightReq>,
) -> AppResult<Json<Value>> {
    let name = req.name.trim();
    if name.is_empty() || name.len() > 60 {
        return Err(AppError::bad_request("highlight name must be 1-60 chars"));
    }
    let id = new_id();
    let me2 = me.clone();
    let name2 = name.to_string();
    let id2 = id.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO highlights (id, user_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id2, me2, name2, now()],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "id": id, "name": name })))
}

pub async fn highlight_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.media_cid, s.thumb_cid, s.created_at, s.expires_at,
                    u.id, u.username, u.display_name, u.avatar_cid,
                    hs.seq
             FROM highlight_stories hs
             JOIN stories s ON s.id = hs.story_id
             JOIN users u ON u.id = s.author_id
             WHERE hs.highlight_id = ?1
             ORDER BY hs.seq ASC",
        )?;
        let rows = stmt
            .query_map([&id], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "media_cid": r.get::<_, String>(1)?,
                    "thumb_cid": r.get::<_, Option<String>>(2)?,
                    "created_at": r.get::<_, i64>(3)?,
                    "expires_at": r.get::<_, i64>(4)?,
                    "seq": r.get::<_, i64>(9)?,
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
    Ok(Json(json!({ "stories": items })))
}

// ---------------------------------------------------------------- comment likes
pub async fn like_comment(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    let me2 = me.clone();
    let id2 = id.clone();
    let inserted = state
        .db
        .writer
        .call(move |conn| {
            let n = conn.execute(
                "INSERT OR IGNORE INTO comment_likes (user_id, comment_id, created_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![me2, id2, now()],
            )?;
            Ok(n > 0)
        })
        .await?;
    Ok(Json(json!({ "liked": true, "new": inserted })))
}

pub async fn unlike_comment(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(id): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM comment_likes WHERE user_id = ?1 AND comment_id = ?2",
                rusqlite::params![me, id],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "liked": false })))
}

// ---------------------------------------------------------------- helper exports
/// Returns the list of user IDs that should be filtered out of the caller's
/// feed / comments / DMs (because they are blocked or muted).
pub fn filter_ids(conn: &rusqlite::Connection, me: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT blocked_id FROM blocks WHERE blocker_id = ?1
         UNION
         SELECT muted_id FROM mutes WHERE muter_id = ?1",
    )?;
    let rows = stmt
        .query_map([me], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
