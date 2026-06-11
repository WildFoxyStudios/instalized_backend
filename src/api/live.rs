//! /v1/live/* — decentralized live streaming control plane (spec §12).
//! The server never sees video: hosts announce 3 s chunk CIDs (WS or REST),
//! the hub fans them out, viewers fetch from the swarm/gateways into MSE.

use crate::auth::AuthUser;
use crate::db::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::ws::protocol;
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct CreateStreamReq {
    pub title: Option<String>,
}

pub async fn create_stream(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<CreateStreamReq>,
) -> AppResult<Json<Value>> {
    let id = new_id();
    let title = req.title.unwrap_or_default();
    if title.len() > 120 {
        return Err(AppError::bad_request("title too long (max 120)"));
    }
    let started = now();
    let id2 = id.clone();
    let me2 = me.clone();
    let title2 = title.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO live_streams (id, host_id, title, status, started_at)
                 VALUES (?1, ?2, ?3, 'live', ?4)",
                rusqlite::params![id2, me2, title2, started],
            )?;
            Ok(())
        })
        .await?;

    // Tell online followers (offline ones discover it on next app open).
    let me3 = me.clone();
    let followers: Vec<String> = state.db.read.with(move |conn| {
        let mut stmt =
            conn.prepare("SELECT follower_id FROM follows WHERE followee_id = ?1")?;
        let rows = stmt
            .query_map([&me3], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })?;
    let announcement = protocol::msg(
        "live.start",
        json!({ "stream_id": id, "host_id": me, "title": title }),
    );
    for f in followers {
        state.hub.send_to_user(&f, &announcement).await;
    }

    // The host implicitly joins its own room.
    state.hub.join_room(&format!("live:{id}"), &me).await;
    Ok(Json(json!({
        "id": id, "host_id": me, "title": title, "status": "live", "started_at": started,
    })))
}

#[derive(Deserialize)]
pub struct ChunkReq {
    pub seq: i64,
    pub cid: String,
    pub duration_ms: Option<i64>,
}

/// REST fallback for chunk announcement (primary path is WS `live.chunk`).
pub async fn post_chunk(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(stream_id): Path<String>,
    Json(req): Json<ChunkReq>,
) -> AppResult<Json<Value>> {
    let chunk = announce_chunk_core(
        &state,
        &me,
        &stream_id,
        req.seq,
        &req.cid,
        req.duration_ms.unwrap_or(3000),
    )
    .await?;
    Ok(Json(chunk))
}

/// Shared by WS and REST. Verifies host, persists the chunk, pins the FIRST chunk
/// (manifesto: Pinata on first chunk), fans out to the room.
pub async fn announce_chunk_core(
    state: &AppState,
    user_id: &str,
    stream_id: &str,
    seq: i64,
    cid: &str,
    duration_ms: i64,
) -> AppResult<Value> {
    if seq < 0 {
        return Err(AppError::bad_request("seq must be >= 0"));
    }
    crate::api::validate_cid(cid)?;
    let sid = stream_id.to_string();
    let uid = user_id.to_string();
    let cid2 = cid.to_string();
    let (inserted, is_first) = state
        .db
        .writer
        .call(move |conn| {
            let (host, status): (String, String) = conn
                .query_row(
                    "SELECT host_id, status FROM live_streams WHERE id = ?1",
                    [&sid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(|_| AppError::not_found("stream not found"))?;
            if host != uid {
                return Err(AppError::forbidden("only the host announces chunks"));
            }
            if status != "live" {
                return Err(AppError::bad_request("stream has ended"));
            }
            let existing: i64 = conn.query_row(
                "SELECT COUNT(*) FROM live_chunks WHERE stream_id = ?1",
                [&sid],
                |r| r.get(0),
            )?;
            let n = conn.execute(
                "INSERT OR IGNORE INTO live_chunks (stream_id, seq, cid, duration_ms, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![sid, seq, cid2, duration_ms, now()],
            )?;
            Ok((n > 0, n > 0 && existing == 0))
        })
        .await?;

    if is_first {
        // Durable origin for late joiners from the very first second (spec §12).
        crate::pinning::enqueue(&state.db, cid, "live_first").await?;
    }

    let chunk = json!({
        "stream_id": stream_id, "seq": seq, "cid": cid, "duration_ms": duration_ms,
    });
    if inserted {
        state
            .hub
            .broadcast_room(
                &format!("live:{stream_id}"),
                &protocol::msg("live.chunk", chunk.clone()),
                Some(user_id),
            )
            .await;
    }
    Ok(chunk)
}

pub async fn end_stream(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Path(stream_id): Path<String>,
) -> AppResult<Json<Value>> {
    let sid = stream_id.clone();
    state
        .db
        .writer
        .call(move |conn| {
            let n = conn.execute(
                "UPDATE live_streams SET status = 'ended', ended_at = ?1
                 WHERE id = ?2 AND host_id = ?3 AND status = 'live'",
                rusqlite::params![now(), sid, me],
            )?;
            if n == 0 {
                return Err(AppError::not_found("live stream not found or not yours"));
            }
            Ok(())
        })
        .await?;
    let room = format!("live:{stream_id}");
    state
        .hub
        .broadcast_room(
            &room,
            &protocol::msg("live.end", json!({ "stream_id": stream_id })),
            None,
        )
        .await;
    state.hub.close_room(&room).await;
    Ok(Json(json!({ "status": "ended" })))
}

/// Stream metadata + full chunk playlist (late join catch-up and VOD assembly).
pub async fn get_stream(
    State(state): State<AppState>,
    AuthUser(_me): AuthUser,
    Path(stream_id): Path<String>,
) -> AppResult<Json<Value>> {
    let result = state.db.read.with(move |conn| {
        let stream = conn
            .query_row(
                "SELECT s.id, s.title, s.status, s.started_at, s.ended_at,
                        u.id, u.username, u.display_name, u.avatar_cid
                 FROM live_streams s JOIN users u ON u.id = s.host_id
                 WHERE s.id = ?1",
                [&stream_id],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "status": r.get::<_, String>(2)?,
                        "started_at": r.get::<_, i64>(3)?,
                        "ended_at": r.get::<_, Option<i64>>(4)?,
                        "host": {
                            "id": r.get::<_, String>(5)?,
                            "username": r.get::<_, String>(6)?,
                            "display_name": r.get::<_, Option<String>>(7)?,
                            "avatar_cid": r.get::<_, Option<String>>(8)?,
                        },
                    }))
                },
            )
            .map_err(|_| AppError::not_found("stream not found"))?;
        let mut stmt = conn.prepare(
            "SELECT seq, cid, duration_ms FROM live_chunks WHERE stream_id = ?1 ORDER BY seq",
        )?;
        let chunks = stmt
            .query_map([&stream_id], |r| {
                Ok(json!({
                    "seq": r.get::<_, i64>(0)?,
                    "cid": r.get::<_, String>(1)?,
                    "duration_ms": r.get::<_, i64>(2)?,
                }))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!({ "stream": stream, "chunks": chunks }))
    })?;
    Ok(Json(result))
}
