//! Authentication: JWT access tokens, Argon2id passwords, Google id_token verify,
//! rotating refresh tokens. The `AuthUser` extractor guards every private route.

pub mod google;
pub mod jwt;
pub mod password;

use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use base64::Engine;
use sha2::{Digest, Sha256};

/// Authenticated user id, extracted from `Authorization: Bearer <jwt>`.
#[derive(Debug, Clone)]
pub struct AuthUser(pub String);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> AppResult<Self> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| AppError::unauthorized("missing bearer token"))?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(|| AppError::unauthorized("malformed authorization header"))?;
        let claims = jwt::verify(&state.cfg.jwt_secret, token)?;
        Ok(AuthUser(claims.sub))
    }
}

/// Optional authentication: anonymous requests yield `MaybeUser(None)`.
/// Used by public read endpoints so SvelteKit SSR (and logged-out humans)
/// can fetch posts/profiles while `liked_by_me`/privacy degrade gracefully.
#[derive(Debug, Clone)]
pub struct MaybeUser(pub Option<String>);

impl FromRequestParts<AppState> for MaybeUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> AppResult<Self> {
        let user = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| h.strip_prefix("Bearer "))
            .and_then(|t| jwt::verify(&state.cfg.jwt_secret, t).ok())
            .map(|c| c.sub);
        Ok(MaybeUser(user))
    }
}

pub fn sha256_hex(data: &str) -> String {
    let digest = Sha256::digest(data.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
