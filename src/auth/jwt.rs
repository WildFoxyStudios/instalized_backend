//! HS256 access tokens, 15 min TTL (spec §4).

use crate::error::{AppError, AppResult};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
}

pub fn issue(secret: &str, user_id: &str, ttl_secs: i64) -> AppResult<String> {
    let now = crate::db::now();
    let claims = Claims {
        sub: user_id.to_string(),
        iat: now,
        exp: now + ttl_secs,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AppError::internal(format!("jwt encode: {e}")))
}

pub fn verify(secret: &str, token: &str) -> AppResult<Claims> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .map(|d| d.claims)
    .map_err(|_| AppError::unauthorized("invalid or expired token"))
}
