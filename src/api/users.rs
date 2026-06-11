//! /v1/users/* — profiles, edit, follow graph, user post grid.

use crate::api::{cursor_of, post_from_row, Page, POST_COLS};
use crate::auth::{AuthUser, MaybeUser};
use crate::db::now;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

fn profile_json(state: &AppState, user_id: &str, viewer_id: &str) -> AppResult<Value> {
    let uid = user_id.to_string();
    let vid = viewer_id.to_string();
    state.db.read.with(move |conn| {
        let (id, username, display_name, bio, avatar_cid, is_private, created_at) = conn
            .query_row(
                "SELECT id, username, display_name, bio, avatar_cid, is_private, created_at
                 FROM users WHERE id = ?1",
                [&uid],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                },
            )?;
        let posts: i64 = conn.query_row(
            "SELECT COUNT(*) FROM posts WHERE author_id = ?1 AND deleted_at IS NULL",
            [&uid],
            |r| r.get(0),
        )?;
        let followers: i64 = conn.query_row(
            "SELECT COUNT(*) FROM follows WHERE followee_id = ?1",
            [&uid],
            |r| r.get(0),
        )?;
        let following: i64 = conn.query_row(
            "SELECT COUNT(*) FROM follows WHERE follower_id = ?1",
            [&uid],
            |r| r.get(0),
        )?;
        let is_following: bool = conn.query_row(
            "SELECT COUNT(*) FROM follows WHERE follower_id = ?1 AND followee_id = ?2",
            [&vid, &uid],
            |r| r.get::<_, i64>(0).map(|n| n > 0),
        )?;
        Ok(json!({
            "id": id,
            "username": username,
            "display_name": display_name,
            "bio": bio,
            "avatar_cid": avatar_cid,
            "is_private": is_private != 0,
            "created_at": created_at,
            "counts": { "posts": posts, "followers": followers, "following": following },
            "is_following": is_following,
            "is_me": uid == vid,
        }))
    })
}

pub async fn get_me(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<Value>> {
    Ok(Json(profile_json(&state, &me, &me)?))
}

#[derive(Deserialize)]
pub struct PatchMeReq {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub avatar_cid: Option<String>,
    pub is_private: Option<bool>,
}

pub async fn patch_me(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<PatchMeReq>,
) -> AppResult<Json<Value>> {
    if let Some(ref bio) = req.bio {
        if bio.len() > 500 {
            return Err(AppError::bad_request("bio too long (max 500)"));
        }
    }
    if let Some(ref name) = req.display_name {
        if name.len() > 80 {
            return Err(AppError::bad_request("display_name too long (max 80)"));
        }
    }
    if let Some(ref cid) = req.avatar_cid {
        crate::api::validate_cid(cid)?;
    }
    let me2 = me.clone();
    state
        .db
        .writer
        .call(move |conn| {
            if let Some(v) = req.display_name {
                conn.execute(
                    "UPDATE users SET display_name = ?1 WHERE id = ?2",
                    rusqlite::params![v, me2],
                )?;
            }
            if let Some(v) = req.bio {
                conn.execute(
                    "UPDATE users SET bio = ?1 WHERE id = ?2",
                    rusqlite::params![v, me2],
                )?;
            }
            if let Some(v) = req.avatar_cid {
                conn.execute(
                    "UPDATE users SET avatar_cid = ?1 WHERE id = ?2",
                    rusqlite::params![v, me2],
                )?;
            }
            if let Some(v) = req.is_private {
                conn.execute(
                    "UPDATE users SET is_private = ?1 WHERE id = ?2",
                    rusqlite::params![v as i64, me2],
                )?;
            }
            Ok(())
        })
        .await?;
    Ok(Json(profile_json(&state, &me, &me)?))
}

pub(crate) fn user_id_by_username(state: &AppState, username: &str) -> AppResult<String> {
    let username = username.to_lowercase();
    state.db.read.with(move |conn| {
        conn.query_row("SELECT id FROM users WHERE username = ?1", [&username], |r| {
            r.get(0)
        })
        .map_err(|_| AppError::not_found("user not found"))
    })
}

pub async fn get_user(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Path(username): Path<String>,
) -> AppResult<Json<Value>> {
    let me = me.unwrap_or_default();
    let uid = user_id_by_username(&state, &username)?;
    Ok(Json(profile_json(&state, &uid, &me)?))
}

/// Profile grid. Private accounts hide the grid from non-followers (`locked: true`)
/// — API-level enforcement; see spec §15 for the CID caveat.
pub async fn user_posts(
    State(state): State<AppState>,
    MaybeUser(me): MaybeUser,
    Path(username): Path<String>,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let me = me.unwrap_or_default();
    let uid = user_id_by_username(&state, &username)?;
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let me2 = me.clone();
    let uid2 = uid.clone();

    let (locked, items): (bool, Vec<Value>) = state.db.read.with(move |conn| {
        let is_private: i64 =
            conn.query_row("SELECT is_private FROM users WHERE id = ?1", [&uid2], |r| {
                r.get(0)
            })?;
        if is_private != 0 && uid2 != me2 {
            let following: i64 = conn.query_row(
                "SELECT COUNT(*) FROM follows WHERE follower_id = ?1 AND followee_id = ?2",
                [&me2, &uid2],
                |r| r.get(0),
            )?;
            if following == 0 {
                return Ok((true, vec![]));
            }
        }
        let sql = format!(
            "SELECT {POST_COLS} FROM posts p
             JOIN users u ON u.id = p.author_id
             LEFT JOIN likes l ON l.post_id = p.id AND l.user_id = ?1
             LEFT JOIN saved_posts sv ON sv.post_id = p.id AND sv.user_id = ?1
             WHERE p.author_id = ?2 AND p.deleted_at IS NULL
               AND (p.created_at < ?3 OR (p.created_at = ?3 AND p.id < ?4))
             ORDER BY p.created_at DESC, p.id DESC LIMIT ?5"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                rusqlite::params![me2, uid2, cur_ts, cur_id, limit],
                post_from_row,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((false, rows))
    })?;

    let next_cursor = next_cursor_from(&items, limit);
    Ok(Json(json!({ "locked": locked, "items": items, "next_cursor": next_cursor })))
}

pub(crate) fn next_cursor_from(items: &[Value], limit: i64) -> Option<String> {
    if items.len() < limit as usize {
        return None;
    }
    let last = items.last()?;
    Some(cursor_of(
        last.get("created_at")?.as_i64()?,
        last.get("id")?.as_str()?,
    ))
}

pub async fn follow(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    if me == target {
        return Err(AppError::bad_request("cannot follow yourself"));
    }
    let me2 = me.clone();
    let target2 = target.clone();
    let inserted = state
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
            let n = conn.execute(
                "INSERT OR IGNORE INTO follows (follower_id, followee_id, created_at) VALUES (?1, ?2, ?3)",
                rusqlite::params![me2, target2, now()],
            )?;
            Ok(n > 0)
        })
        .await?;
    if inserted {
        crate::api::notifications::notify(&state, &target, "follow", &me, None).await;
    }
    Ok(Json(json!({ "following": true })))
}

pub async fn unfollow(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(target): Path<String>,
) -> AppResult<Json<Value>> {
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "DELETE FROM follows WHERE follower_id = ?1 AND followee_id = ?2",
                rusqlite::params![me, target],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "following": false })))
}
