//! /v1/auth/* — register, login, google, refresh (rotating), logout.

use crate::auth::{password, random_token, sha256_hex};
use crate::db::{new_id, now};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
pub struct RegisterReq {
    pub email: String,
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct GoogleReq {
    pub id_token: String,
}

#[derive(Deserialize)]
pub struct RefreshReq {
    pub refresh_token: String,
}

fn validate_username(u: &str) -> AppResult<String> {
    let u = u.trim().to_lowercase();
    let ok = (3..=30).contains(&u.len())
        && u.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_');
    if ok {
        Ok(u)
    } else {
        Err(AppError::bad_request(
            "username must be 3-30 chars of a-z 0-9 . _",
        ))
    }
}

async fn issue_pair(state: &AppState, user_id: &str) -> AppResult<Json<Value>> {
    let access = crate::auth::jwt::issue(&state.cfg.jwt_secret, user_id, state.cfg.access_ttl_secs)?;
    let refresh = random_token();
    let hash = sha256_hex(&refresh);
    let uid = user_id.to_string();
    let expires = now() + state.cfg.refresh_ttl_secs;
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![new_id(), uid, hash, expires],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({
        "access_token": access,
        "refresh_token": refresh,
        "token_type": "Bearer",
        "expires_in": state.cfg.access_ttl_secs,
        "user_id": user_id,
    })))
}

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterReq>,
) -> AppResult<Json<Value>> {
    let email = req.email.trim().to_lowercase();
    if !email.contains('@') || email.len() > 254 {
        return Err(AppError::bad_request("invalid email"));
    }
    let username = validate_username(&req.username)?;
    if req.password.len() < 8 || req.password.len() > 128 {
        return Err(AppError::bad_request("password must be 8-128 chars"));
    }
    let hash = password::hash_blocking(req.password).await?;
    let user_id = new_id();
    let uid = user_id.clone();
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "INSERT INTO users (id, email, password_hash, username, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![uid, email, hash, username, now()],
            )?;
            Ok(())
        })
        .await?;
    issue_pair(&state, &user_id).await
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginReq>,
) -> AppResult<Json<Value>> {
    let email = req.email.trim().to_lowercase();
    let row: Option<(String, Option<String>)> = state.db.read.with(move |conn| {
        let r = conn
            .query_row(
                "SELECT id, password_hash FROM users WHERE email = ?1",
                [&email],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        Ok(r)
    })?;
    let Some((user_id, Some(hash))) = row else {
        // Constant-ish time: still run a verify against a dummy hash.
        let _ = password::verify_blocking(
            "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
            req.password,
        )
        .await;
        return Err(AppError::unauthorized("invalid credentials"));
    };
    if !password::verify_blocking(hash, req.password).await? {
        return Err(AppError::unauthorized("invalid credentials"));
    }
    issue_pair(&state, &user_id).await
}

pub async fn google(
    State(state): State<AppState>,
    Json(req): Json<GoogleReq>,
) -> AppResult<Json<Value>> {
    let Some(verifier) = state.google.as_ref() else {
        return Err(AppError::unavailable("google auth not configured"));
    };
    let claims = verifier.verify(&req.id_token).await?;
    let sub = claims.sub.clone();
    let email = claims.email.clone().map(|e| e.to_lowercase());
    let name = claims.name.clone();
    let user_id = state
        .db
        .writer
        .call(move |conn| {
            // 1. Already linked?
            if let Ok(id) = conn.query_row(
                "SELECT id FROM users WHERE google_sub = ?1",
                [&sub],
                |r| r.get::<_, String>(0),
            ) {
                return Ok(id);
            }
            // 2. Same email → link accounts.
            if let Some(ref e) = email {
                if let Ok(id) = conn.query_row(
                    "SELECT id FROM users WHERE email = ?1",
                    [e],
                    |r| r.get::<_, String>(0),
                ) {
                    conn.execute(
                        "UPDATE users SET google_sub = ?1 WHERE id = ?2",
                        rusqlite::params![sub, id],
                    )?;
                    return Ok(id);
                }
            }
            // 3. New user with a derived, conflict-proof username.
            let base: String = email
                .as_deref()
                .and_then(|e| e.split('@').next())
                .unwrap_or("user")
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '_')
                .take(20)
                .collect::<String>()
                .to_lowercase();
            let id = new_id();
            let suffix = &id[id.len() - 4..];
            let username = format!("{}_{}", if base.len() >= 3 { base } else { "user".into() }, suffix.to_lowercase());
            conn.execute(
                "INSERT INTO users (id, email, google_sub, username, display_name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![id, email, sub, username, name, now()],
            )?;
            Ok(id)
        })
        .await?;
    issue_pair(&state, &user_id).await
}

pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> AppResult<Json<Value>> {
    let hash = sha256_hex(&req.refresh_token);
    let user_id = state
        .db
        .writer
        .call(move |conn| {
            let (id, user_id, expires_at, revoked_at): (String, String, i64, Option<i64>) = conn
                .query_row(
                    "SELECT id, user_id, expires_at, revoked_at FROM refresh_tokens WHERE token_hash = ?1",
                    [&hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .map_err(|_| AppError::unauthorized("invalid refresh token"))?;
            if revoked_at.is_some() || expires_at < now() {
                return Err(AppError::unauthorized("expired refresh token"));
            }
            // Rotation: the presented token is single-use.
            conn.execute(
                "UPDATE refresh_tokens SET revoked_at = ?1 WHERE id = ?2",
                rusqlite::params![now(), id],
            )?;
            Ok(user_id)
        })
        .await?;
    issue_pair(&state, &user_id).await
}

pub async fn logout(
    State(state): State<AppState>,
    Json(req): Json<RefreshReq>,
) -> AppResult<Json<Value>> {
    let hash = sha256_hex(&req.refresh_token);
    state
        .db
        .writer
        .call(move |conn| {
            conn.execute(
                "UPDATE refresh_tokens SET revoked_at = ?1 WHERE token_hash = ?2 AND revoked_at IS NULL",
                rusqlite::params![now(), hash],
            )?;
            Ok(())
        })
        .await?;
    Ok(Json(json!({ "ok": true })))
}
