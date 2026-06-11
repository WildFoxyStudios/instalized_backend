//! /v1/media/* — CID announcement (creates pin jobs) and pin status polling.

use crate::auth::AuthUser;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct AnnounceReq {
    pub cid: String,
    pub kind: Option<String>,
}

pub async fn announce(
    State(state): State<AppState>,
    AuthUser(_me): AuthUser,
    Json(req): Json<AnnounceReq>,
) -> AppResult<Json<Value>> {
    crate::api::validate_cid(&req.cid)?;
    let kind = req.kind.unwrap_or_else(|| "media".to_string());
    if kind.len() > 32 {
        return Err(AppError::bad_request("kind too long"));
    }
    crate::pinning::enqueue(&state.db, &req.cid, &kind).await?;
    pin_status(State(state), Path(req.cid)).await
}

/// Hands the browser a scoped upload credential (web has no embedded IPFS node;
/// Flutter uses its native node instead). 503 until PINATA_UPLOAD_JWT is set.
pub async fn upload_token(
    State(state): State<AppState>,
    AuthUser(_me): AuthUser,
) -> AppResult<Json<Value>> {
    match &state.cfg.pinata_upload_jwt {
        Some(jwt) => Ok(Json(json!({
            "provider": "pinata",
            "endpoint": "https://api.pinata.cloud/pinning/pinFileToIPFS",
            "jwt": jwt,
        }))),
        None => Err(AppError::unavailable("web uploads not configured (PINATA_UPLOAD_JWT)")),
    }
}

pub async fn pin_status(
    State(state): State<AppState>,
    Path(cid): Path<String>,
) -> AppResult<Json<Value>> {
    let row = state.db.read.with(move |conn| {
        conn.query_row(
            "SELECT cid, kind, status, attempts, created_at, pinned_at FROM pin_jobs WHERE cid = ?1",
            [&cid],
            |r| {
                Ok(json!({
                    "cid": r.get::<_, String>(0)?,
                    "kind": r.get::<_, String>(1)?,
                    "status": r.get::<_, String>(2)?,
                    "attempts": r.get::<_, i64>(3)?,
                    "created_at": r.get::<_, i64>(4)?,
                    "pinned_at": r.get::<_, Option<i64>>(5)?,
                }))
            },
        )
        .map_err(|_| AppError::not_found("no pin job for cid"))
    })?;
    Ok(Json(row))
}
