//! /v1/dm/* — direct messages. Text relays through the server (never P2P, spec §15);
//! realtime via WS, history via REST. `send_message_core` is shared with the WS path.

use crate::api::Page;
use crate::auth::AuthUser;
use crate::db::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::ws::protocol;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

fn canonical_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

pub async fn threads_list(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<Value>> {
    let items: Vec<Value> = state.db.read.with(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT t.id,
                    peer.id, peer.username, peer.display_name, peer.avatar_cid,
                    (SELECT body FROM dm_messages m WHERE m.thread_id = t.id
                     ORDER BY m.created_at DESC, m.id DESC LIMIT 1),
                    (SELECT created_at FROM dm_messages m WHERE m.thread_id = t.id
                     ORDER BY m.created_at DESC, m.id DESC LIMIT 1),
                    (SELECT COUNT(*) FROM dm_messages m WHERE m.thread_id = t.id
                     AND m.sender_id != ?1 AND m.read_at IS NULL)
             FROM dm_threads t
             JOIN users peer ON peer.id = CASE WHEN t.user_a = ?1 THEN t.user_b ELSE t.user_a END
             WHERE t.user_a = ?1 OR t.user_b = ?1
             ORDER BY COALESCE((SELECT created_at FROM dm_messages m WHERE m.thread_id = t.id
                                ORDER BY m.created_at DESC LIMIT 1), t.created_at) DESC",
        )?;
        let rows = stmt
            .query_map([&me], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "peer": {
                        "id": r.get::<_, String>(1)?,
                        "username": r.get::<_, String>(2)?,
                        "display_name": r.get::<_, Option<String>>(3)?,
                        "avatar_cid": r.get::<_, Option<String>>(4)?,
                    },
                    "last_message": r.get::<_, Option<String>>(5)?,
                    "last_message_at": r.get::<_, Option<i64>>(6)?,
                    "unread": r.get::<_, i64>(7)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Deserialize)]
pub struct ThreadCreateReq {
    pub user_id: String,
}

pub async fn thread_create(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<ThreadCreateReq>,
) -> AppResult<Json<Value>> {
    let (thread_id, peer) = get_or_create_thread(&state, &me, &req.user_id).await?;
    Ok(Json(json!({ "id": thread_id, "peer_id": peer })))
}

async fn get_or_create_thread(
    state: &AppState,
    me: &str,
    other: &str,
) -> AppResult<(String, String)> {
    if me == other {
        return Err(AppError::bad_request("cannot DM yourself"));
    }
    let (a, b) = canonical_pair(me, other);
    let other2 = other.to_string();
    let thread_id = state
        .db
        .writer
        .call(move |conn| {
            let exists: i64 =
                conn.query_row("SELECT COUNT(*) FROM users WHERE id = ?1", [&other2], |r| {
                    r.get(0)
                })?;
            if exists == 0 {
                return Err(AppError::not_found("user not found"));
            }
            conn.execute(
                "INSERT OR IGNORE INTO dm_threads (id, user_a, user_b, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![new_id(), a, b, now()],
            )?;
            let id: String = conn.query_row(
                "SELECT id FROM dm_threads WHERE user_a = ?1 AND user_b = ?2",
                rusqlite::params![a, b],
                |r| r.get(0),
            )?;
            Ok(id)
        })
        .await?;
    Ok((thread_id, other.to_string()))
}

/// Membership check returning the peer's id.
fn thread_peer(conn: &rusqlite::Connection, thread_id: &str, me: &str) -> AppResult<String> {
    let (a, b): (String, String) = conn
        .query_row(
            "SELECT user_a, user_b FROM dm_threads WHERE id = ?1",
            [thread_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| AppError::not_found("thread not found"))?;
    if a == me {
        Ok(b)
    } else if b == me {
        Ok(a)
    } else {
        Err(AppError::forbidden("not a member of this thread"))
    }
}

pub async fn messages_list(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(thread_id): Path<String>,
    Query(page): Query<Page>,
) -> AppResult<Json<Value>> {
    let (cur_ts, cur_id) = page.keyset();
    let limit = page.limit();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        thread_peer(conn, &thread_id, &me)?;
        let mut stmt = conn.prepare(
            "SELECT id, sender_id, body, media_cid, created_at, read_at,
                    kind, duration_ms, waveform, thumb_cid, width, height
             FROM dm_messages
             WHERE thread_id = ?1
               AND (created_at < ?2 OR (created_at = ?2 AND id < ?3))
             ORDER BY created_at DESC, id DESC LIMIT ?4",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![thread_id, cur_ts, cur_id, limit], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "sender_id": r.get::<_, String>(1)?,
                    "body": r.get::<_, String>(2)?,
                    "media_cid": r.get::<_, Option<String>>(3)?,
                    "created_at": r.get::<_, i64>(4)?,
                    "read_at": r.get::<_, Option<i64>>(5)?,
                    "kind": r.get::<_, String>(6)?,
                    "duration_ms": r.get::<_, Option<i64>>(7)?,
                    "waveform": r.get::<_, Option<String>>(8)?,
                    "thumb_cid": r.get::<_, Option<String>>(9)?,
                    "width": r.get::<_, Option<i64>>(10)?,
                    "height": r.get::<_, Option<i64>>(11)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let next_cursor = crate::api::users::next_cursor_from(&items, limit);
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

/// Validated message-create payload.
///
/// Kind is required to be one of `text|image|voice` (future: `video|reel`).
/// `duration_ms` is required for `voice`. `waveform` is a JSON-encoded
/// array of small integers (≤200 samples) that the sender computed
/// client-side; the backend stores it verbatim and the receiver renders
/// the playback scrubber from it without having to decode audio first.
#[derive(Deserialize)]
pub struct MessageReq {
    pub body: Option<String>,
    pub media_cid: Option<String>,
    /// Defaults to "text" when the payload is body-only.
    pub kind: Option<String>,
    pub duration_ms: Option<i64>,
    pub waveform: Option<String>,
    pub thumb_cid: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

pub async fn message_create(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(thread_id): Path<String>,
    Json(req): Json<MessageReq>,
) -> AppResult<Json<Value>> {
    let message = send_message_core(
        &state,
        &me,
        None,
        Some(thread_id),
        req,
    )
    .await?;
    Ok(Json(message))
}

/// Shared by REST and WS: resolve thread, persist, fan out to the recipient.
pub async fn send_message_core(
    state: &AppState,
    sender: &str,
    to_user: Option<String>,
    thread_id: Option<String>,
    req: MessageReq,
) -> AppResult<Value> {
    let body = req.body.unwrap_or_default().trim().to_string();
    let media_cid = req.media_cid;
    if body.is_empty() && media_cid.is_none() {
        return Err(AppError::bad_request("message needs body or media_cid"));
    }
    if body.len() > 4000 {
        return Err(AppError::bad_request("message too long (max 4000)"));
    }
    if let Some(ref c) = media_cid {
        crate::api::validate_cid(c)?;
    }
    if let Some(ref c) = req.thumb_cid {
        crate::api::validate_cid(c)?;
    }
    let kind = req.kind.unwrap_or_else(|| "text".to_string());
    if !matches!(kind.as_str(), "text" | "image" | "voice" | "video" | "reel") {
        return Err(AppError::bad_request(format!("unknown kind: {kind}")));
    }
    if kind == "voice" {
        match req.duration_ms {
            Some(0..=600_000) => {} // ≤ 10 min
            _ => return Err(AppError::bad_request("voice needs duration_ms 1..=600000")),
        }
        if req.waveform.as_deref().map_or(true, |s| s.is_empty()) {
            return Err(AppError::bad_request("voice needs waveform"));
        }
    }
    if matches!(kind.as_str(), "image" | "video" | "reel") && media_cid.is_none() {
        return Err(AppError::bad_request("media message needs media_cid"));
    }

    // Resolve the thread + peer.
    let (thread_id, peer) = match (thread_id, to_user) {
        (Some(tid), _) => {
            let tid2 = tid.clone();
            let me2 = sender.to_string();
            let peer = state
                .db
                .read
                .with(move |conn| thread_peer(conn, &tid2, &me2))?;
            (tid, peer)
        }
        (None, Some(to)) => get_or_create_thread(state, sender, &to).await?,
        (None, None) => return Err(AppError::bad_request("thread_id or to_user_id required")),
    };

    let id = new_id();
    let created = now();
    let id2 = id.clone();
    let tid2 = thread_id.clone();
    let sender2 = sender.to_string();
    let body2 = body.clone();
    let media2 = media_cid.clone();
    let kind2 = kind.clone();
    let duration2 = req.duration_ms;
    let waveform2 = req.waveform.clone();
    let thumb2 = req.thumb_cid.clone();
    let width2 = req.width;
    let height2 = req.height;
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO dm_messages
                   (id, thread_id, sender_id, body, media_cid, created_at,
                    kind, duration_ms, waveform, thumb_cid, width, height)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                rusqlite::params![
                    id2, tid2, sender2, body2, media2, created,
                    kind2, duration2, waveform2, thumb2, width2, height2
                ],
            )?;
            Ok(())
        })
        .await?;

    let message = json!({
        "id": id, "thread_id": thread_id, "sender_id": sender,
        "body": body, "media_cid": media_cid, "created_at": created, "read_at": null,
        "kind": kind, "duration_ms": req.duration_ms, "waveform": req.waveform,
        "thumb_cid": req.thumb_cid, "width": req.width, "height": req.height,
    });

    // Realtime to the recipient; offline → push notification path (manifesto #5).
    let delivered = state
        .hub
        .send_to_user(&peer, &protocol::msg("dm.new", message.clone()))
        .await;
    if !delivered {
        tracing::debug!("dm recipient {peer} offline — push notification would fire here");
    }
    Ok(message)
}

#[derive(Deserialize)]
pub struct ReadReq {
    pub upto_id: String,
}

pub async fn mark_read(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(thread_id): Path<String>,
    Json(req): Json<ReadReq>,
) -> AppResult<Json<Value>> {
    let n = mark_read_core(&state, &me, &thread_id, &req.upto_id).await?;
    Ok(Json(json!({ "read": n })))
}

pub async fn mark_read_core(
    state: &AppState,
    me: &str,
    thread_id: &str,
    upto_id: &str,
) -> AppResult<i64> {
    let me2 = me.to_string();
    let tid = thread_id.to_string();
    let upto = upto_id.to_string();
    let (n, peer) = state
        .db
        .writer
        .call(move |conn| {
            let peer = thread_peer(conn, &tid, &me2)?;
            let n = conn.execute(
                "UPDATE dm_messages SET read_at = ?1
                 WHERE thread_id = ?2 AND sender_id != ?3 AND id <= ?4 AND read_at IS NULL",
                rusqlite::params![now(), tid, me2, upto],
            )?;
            Ok((n as i64, peer))
        })
        .await?;
    if n > 0 {
        state
            .hub
            .send_to_user(
                &peer,
                &protocol::msg(
                    "dm.read",
                    json!({ "thread_id": thread_id, "upto_id": upto_id, "by": me }),
                ),
            )
            .await;
    }
    Ok(n)
}

// ---------------- reactions ----------------
//
// A user can react to any message in a thread they belong to. The reaction
// is a single emoji (we trim + NFC-normalize + cap at 16 chars server-side).
// A user can have at most one reaction per message — re-reacting with the
// same emoji is a toggle (delete), re-reacting with a different emoji is
// a swap (delete old, insert new). Both happen in a single writer call so
// the unique-index invariant cannot be observed mid-flight by other writers.
//
// Realtime: the peer is notified via WS with `dm.reaction` carrying the
// message_id, emoji, user_id, and the resulting aggregate (count for that
// emoji + whether `mine` for the peer). This lets the receiver flip a
// single bubble in place without re-fetching the page.

/// Maximum bytes of a normalized emoji we will accept. 16 is enough for any
/// real emoji (single codepoint, flag, ZWJ family, skin tone) but rejects
/// pasted rivers of ZWJ + variation selectors.
const MAX_EMOJI_LEN: usize = 16;

fn normalize_emoji(raw: &str) -> AppResult<String> {
    // Trim surrounding whitespace.
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::bad_request("emoji required"));
    }
    // NFC normalize so the same visual glyph (decomposed vs. precomposed)
    // shares a single key in the unique index.
    let nfc: String = unicode_normalization::UnicodeNormalization::nfc(trimmed)
        .collect();
    // Enforce length AFTER normalization so a user can't smuggle a long
    // sequence by sneaking combining marks into a short-emoji input.
    if nfc.chars().count() > MAX_EMOJI_LEN {
        return Err(AppError::bad_request(format!(
            "emoji too long (max {MAX_EMOJI_LEN} chars)"
        )));
    }
    Ok(nfc)
}

#[derive(Deserialize)]
pub struct ReactionReq {
    pub emoji: String,
}

/// `PUT /v1/dm/messages/{id}/reaction` — toggle on.
///
/// If the user already has a reaction on this message, the request is a
/// SWAP (the old reaction is removed, the new one inserted). If the new
/// emoji equals the old emoji, the request is a TOGGLE-OFF and the user
/// ends up with no reaction. Either way, the response is the up-to-date
/// aggregate (per-emoji counts and which user is "me") and the WS event
/// fans out to the peer with the same payload.
pub async fn react_to_message(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(message_id): Path<String>,
    Json(req): Json<ReactionReq>,
) -> AppResult<Json<Value>> {
    let emoji = normalize_emoji(&req.emoji)?;
    let (peer, payload) = react_core(&state, &me, &message_id, &emoji).await?;
    let _ = state
        .hub
        .send_to_user(&peer, &protocol::msg("dm.reaction", payload.clone()))
        .await;
    Ok(Json(payload))
}

/// `DELETE /v1/dm/messages/{id}/reaction` — clear my reaction unconditionally.
pub async fn clear_my_reaction(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(message_id): Path<String>,
) -> AppResult<Json<Value>> {
    let peer = {
        let me2 = me.clone();
        let mid = message_id.clone();
        state
            .db
            .writer
            .call(move |conn| {
                // message_peer validates the message belongs to a thread I am in
                // and returns the OTHER member's id (the one we should notify).
                let peer = message_peer(conn, &mid, &me2)?;
                let _n = conn.execute(
                    "DELETE FROM dm_reactions WHERE message_id = ?1 AND user_id = ?2",
                    rusqlite::params![mid, me2],
                )?;
                Ok(peer)
            })
            .await?
    };
    let payload = reaction_payload_for(&state, &message_id, &me).await?;
    let _ = state
        .hub
        .send_to_user(&peer, &protocol::msg("dm.reaction", payload.clone()))
        .await;
    Ok(Json(payload))
}

/// Shared core for the toggle. Returns (peer_id, payload) so the caller
/// can fan out the realtime event with the new aggregate.
async fn react_core(
    state: &AppState,
    me: &str,
    message_id: &str,
    emoji: &str,
) -> AppResult<(String, Value)> {
    let me2 = me.to_string();
    let mid = message_id.to_string();
    let emoji2 = emoji.to_string();
    let (peer, prev_emoji) = state
        .db
        .writer
        .call(move |conn| {
            let peer = message_peer(conn, &mid, &me2)?;
            // Find the user's current reaction, if any.
            let prev: Option<String> = conn
                .query_row(
                    "SELECT emoji FROM dm_reactions WHERE message_id = ?1 AND user_id = ?2",
                    rusqlite::params![mid, me2],
                    |r| r.get(0),
                )
                .ok();
            if let Some(ref old) = prev {
                if old == &emoji2 {
                    // Same emoji → toggle off.
                    conn.execute(
                        "DELETE FROM dm_reactions WHERE message_id = ?1 AND user_id = ?2",
                        rusqlite::params![mid, me2],
                    )?;
                } else {
                    // Different emoji → swap.
                    conn.execute(
                        "UPDATE dm_reactions SET emoji = ?1, created_at = ?2
                          WHERE message_id = ?3 AND user_id = ?4",
                        rusqlite::params![emoji2, now(), mid, me2],
                    )?;
                }
            } else {
                // No prior reaction → insert.
                conn.execute(
                    "INSERT INTO dm_reactions (id, message_id, user_id, emoji, created_at)
                          VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![new_id(), mid, me2, emoji2, now()],
                )?;
            }
            Ok((peer, prev))
        })
        .await?;
    let payload = reaction_payload_for(state, message_id, me).await?;
    // If the user toggled off and the peer already had no notifications to
    // process, we still send so any open client can clear the chip.
    let _ = prev_emoji; // kept for future auditing
    Ok((peer, payload))
}

/// Look up the peer's user_id for a given message, asserting the caller
/// is a thread member. Returns the OTHER member's id (the one we should
/// notify over WS).
fn message_peer(conn: &rusqlite::Connection, message_id: &str, me: &str) -> AppResult<String> {
    let (a, b): (String, String) = conn
        .query_row(
            "SELECT t.user_a, t.user_b
               FROM dm_messages m
               JOIN dm_threads  t ON t.id = m.thread_id
              WHERE m.id = ?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| AppError::not_found("message not found"))?;
    if a == me {
        Ok(b)
    } else if b == me {
        Ok(a)
    } else {
        Err(AppError::forbidden("not a member of this thread"))
    }
}

/// Build the per-emoji aggregate for a message and attach `mine: true` to
/// the emoji the `me` user currently has on it (if any). Used both by the
/// REST response and by the realtime fan-out payload.
///
/// Shape:
/// ```json
/// {
///   "message_id": "01H...",
///   "reactions": [
///     { "emoji": "👍", "count": 2, "users": ["u_alice", "u_bob"], "mine": true  },
///     { "emoji": "🔥", "count": 1, "users": ["u_carol"],         "mine": false }
///   ]
/// }
/// ```
async fn reaction_payload_for(
    state: &AppState,
    message_id: &str,
    me: &str,
) -> AppResult<Value> {
    let me2 = me.to_string();
    let mid = message_id.to_string();
    let items: Vec<Value> = state.db.read.with(move |conn| {
        // Authorize once: any reader that owns/has access to the message can see the aggregate.
        let _peer = message_peer(conn, &mid, &me2)?;
        let mut stmt = conn.prepare(
            "SELECT emoji, user_id, COUNT(*) OVER (PARTITION BY emoji) AS n
               FROM dm_reactions
              WHERE message_id = ?1
              ORDER BY emoji, created_at",
        )?;
        // Group by emoji in Rust: the WINDOW function gives us the per-emoji
        // count on every row, but we want a single entry per emoji with the
        // list of users. Easier to do this in two passes.
        let mut by_emoji: std::collections::BTreeMap<String, (i64, Vec<String>)> =
            std::collections::BTreeMap::new();
        let mut rows = stmt.query(rusqlite::params![mid])?;
        while let Some(r) = rows.next()? {
            let emoji: String = r.get(0)?;
            let user_id: String = r.get(1)?;
            let n: i64 = r.get(2)?;
            by_emoji.entry(emoji).or_insert((n, Vec::new())).1.push(user_id);
        }
        let items: Vec<Value> = by_emoji
            .into_iter()
            .map(|(emoji, (count, mut users))| {
                users.sort();
                let mine = users.iter().any(|u| u == &me2);
                json!({
                    "emoji": emoji,
                    "count": count,
                    "users": users,
                    "mine": mine,
                })
            })
            .collect();
        Ok(items)
    })?;
    Ok(json!({ "message_id": message_id, "reactions": items }))
}
