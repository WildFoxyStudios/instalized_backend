//! Argon2id hashing. Callers run these on `spawn_blocking` — hashing is ~100 ms by design.

use crate::error::{AppError, AppResult};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

pub fn hash(password: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::internal(format!("hash: {e}")))
}

pub fn verify(stored_hash: &str, password: &str) -> bool {
    PasswordHash::new(stored_hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

pub async fn hash_blocking(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || hash(&password))
        .await
        .map_err(|e| AppError::internal(format!("join: {e}")))?
}

pub async fn verify_blocking(stored_hash: String, password: String) -> AppResult<bool> {
    tokio::task::spawn_blocking(move || verify(&stored_hash, &password))
        .await
        .map_err(|e| AppError::internal(format!("join: {e}")))
}
