//! /v1/users/me/2fa/* — TOTP (RFC 6238) over SHA-1 with 30s period, 6 digits.
//! Compatible with Google Authenticator, Authy, 1Password, Bitwarden, etc.
//!
//! Flow:
//!   1. POST /2fa/enable  → returns base32 secret + otpauth:// URI + 10 backup codes.
//!      The secret is persisted in `users.totp_secret` but `totp_enabled` stays 0
//!      until the user proves possession of a valid code.
//!   2. POST /2fa/confirm {code} → sets `totp_enabled=1` if the code matches the
//!      pending secret. Idempotent.
//!   3. POST /2fa/disable {password, code} → clears secret + backup codes. Requires
//!      a fresh TOTP code OR a still-valid backup code (proof the caller knows the
//!      factor) AND the current password (proof the caller knows the credential).
//!   4. Backup codes are single-use, stored as SHA-256 hashes. Each is 10 chars
//!      (XXXXX-XXXXX) from a 32-char alphabet to stay URL-safe and easy to type.
//!
//! Login integration (see `auth.rs`): when a user has `totp_enabled=1` and posts
//! to /v1/auth/login with valid creds, the response carries `requires_2fa: true`
//! and a short-lived `pending_token` (10 min). The client then calls
//! POST /v1/auth/2fa {pending_token, code_or_backup} to mint the access+refresh
//! pair. This way no TOTP code ever touches the access-token lifetime.

use crate::auth::{password, AuthUser};
use crate::db::now;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use base32::{Alphabet, encode as b32};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha1 = Hmac<Sha1>;

const PERIOD: u64 = 30;
const DIGITS: u32 = 6;
const ISSUER: &str = "hybridsocial";
const BACKUP_CODES: usize = 10;
// RFC 4648 base32 + lowercase; both are widely accepted by authenticator apps.
const ALPHABET: Alphabet = Alphabet::Rfc4648 { padding: false };

// ---------- 2FA endpoints (mounted under /v1/users/me/2fa) ----------

#[derive(Serialize)]
pub struct EnableResp {
    /// base32-encoded secret, no padding, ready to type into an authenticator app.
    pub secret: String,
    /// otpauth:// URI (Google Authenticator format). QR codes encode this.
    pub otpauth_uri: String,
    /// Plain-text backup codes, returned ONCE. The user must save them.
    pub backup_codes: Vec<String>,
}

pub async fn enable(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<EnableResp>> {
    // 20 random bytes = 160 bits, the upper bound the RFC considers safe.
    let mut secret_bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut secret_bytes);
    let secret = b32(ALPHABET, &secret_bytes);
    let username: String = state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT username FROM users WHERE id = ?1",
            [&me],
            |r| r.get(0),
        )?)
    })?;
    let uri = format!(
        "otpauth://totp/{label}:{user}?secret={secret}&issuer={label}&algorithm=SHA1&digits={d}&period={p}",
        label = ISSUER, user = urlencoded(&username), secret = secret, d = DIGITS, p = PERIOD
    );
    // Backup codes: 10 random bytes per code, formatted XXXXX-XXXXX (11 chars,
    // uppercase A–Z + 0–9 minus easily-confused I/O/0/1). Stored hashed; the
    // plaintext is returned exactly once to the user.
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789"; // 31 chars, no I/O/0/1
    let mut codes: Vec<String> = Vec::with_capacity(BACKUP_CODES);
    let mut code_hashes: Vec<String> = Vec::with_capacity(BACKUP_CODES);
    let mut buf = [0u8; 10];
    for _ in 0..BACKUP_CODES {
        rand::thread_rng().fill_bytes(&mut buf);
        // Take 10 chars from a 31-symbol alphabet (190 bits of entropy per code).
        let mut s = String::with_capacity(11);
        for i in 0..10 {
            if i == 5 { s.push('-'); }
            s.push(CHARSET[(buf[i] as usize) % CHARSET.len()] as char);
        }
        codes.push(s.clone());
        code_hashes.push(sha256_hex_str(s.to_lowercase().as_bytes()));
    }
    let me_id = me.clone();
    let secret_for_db = secret.clone();
    state.db.writer.call(move |conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE users SET totp_secret = ?1, totp_enabled = 0, totp_verified_at = NULL WHERE id = ?2",
            rusqlite::params![secret_for_db, me_id],
        )?;
        // Replace any prior backup codes — generating a new set means the old
        // ones can no longer be used.
        tx.execute("DELETE FROM user_backup_codes WHERE user_id = ?1", [&me_id])?;
        for h in code_hashes {
            tx.execute(
                "INSERT INTO user_backup_codes (user_id, code_hash, used_at, created_at)
                 VALUES (?1, ?2, NULL, ?3)",
                rusqlite::params![me_id, h, now()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Json(EnableResp { secret, otpauth_uri: uri, backup_codes: codes }))
}

#[derive(Deserialize)]
pub struct ConfirmReq {
    pub code: String,
}

pub async fn confirm(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<ConfirmReq>,
) -> AppResult<Json<Value>> {
    let (secret, already_enabled): (Option<String>, bool) = state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT totp_secret, totp_enabled FROM users WHERE id = ?1",
            [&me],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })?;
    let Some(secret) = secret else {
        return Err(AppError::bad_request("2fa not initiated; call /enable first"));
    };
    if already_enabled {
        return Ok(Json(json!({ "ok": true, "totp_enabled": true, "already": true })));
    }
    if !totp_matches(&secret, &req.code) {
        return Err(AppError::bad_request("invalid totp code"));
    }
    state.db.writer.call(move |conn| {
        conn.execute(
            "UPDATE users SET totp_enabled = 1, totp_verified_at = ?1 WHERE id = ?2",
            rusqlite::params![now(), me],
        )?;
        Ok(())
    }).await?;
    Ok(Json(json!({ "ok": true, "totp_enabled": true })))
}

#[derive(Deserialize)]
pub struct DisableReq {
    pub password: String,
    pub code: String, // TOTP code OR a backup code
}

pub async fn disable(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
    Json(req): Json<DisableReq>,
) -> AppResult<Json<Value>> {
    let (hash, enabled): (Option<String>, bool) = state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT password_hash, totp_enabled FROM users WHERE id = ?1",
            [&me],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })?;
    if !enabled {
        return Err(AppError::bad_request("2fa is not enabled"));
    }
    let hash = hash.ok_or_else(|| AppError::unauthorized("account has no password"))?;
    if !password::verify_blocking(hash, req.password).await? {
        return Err(AppError::unauthorized("invalid password"));
    }
    let secret: String = state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT totp_secret FROM users WHERE id = ?1",
            [&me],
            |r| r.get(0),
        )?)
    })?;
    let totp_ok = totp_matches(&secret, &req.code);
    let backup_ok = if !totp_ok { consume_backup(&state, &me, &req.code).await? } else { false };
    if !totp_ok && !backup_ok {
        return Err(AppError::unauthorized("invalid totp or backup code"));
    }
    state.db.writer.call(move |conn| {
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE users SET totp_enabled = 0, totp_secret = NULL, totp_verified_at = NULL WHERE id = ?1",
            rusqlite::params![me],
        )?;
        tx.execute("DELETE FROM user_backup_codes WHERE user_id = ?1", [&me])?;
        tx.commit()?;
        Ok(())
    }).await?;
    Ok(Json(json!({ "ok": true, "totp_enabled": false })))
}

pub async fn status(
    State(state): State<AppState>,
    AuthUser(me): AuthUser,
) -> AppResult<Json<Value>> {
    let (enabled, verified_at, backup_remaining): (bool, Option<i64>, i64) = state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT totp_enabled, totp_verified_at,
                    (SELECT COUNT(*) FROM user_backup_codes WHERE user_id = ?1 AND used_at IS NULL)
             FROM users WHERE id = ?1",
            [&me],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?)
    })?;
    Ok(Json(json!({
        "totp_enabled": enabled,
        "verified_at": verified_at,
        "backup_codes_remaining": backup_remaining,
    })))
}

// ---------- shared helpers (called from auth.rs on login + 2fa verify) ----------

/// Looks up `totp_enabled` and `totp_secret` for a user id.
pub fn user_factor(
    state: &AppState,
    user_id: &str,
) -> AppResult<(bool, Option<String>)> {
    state.db.read.with(|conn| {
        Ok(conn.query_row(
            "SELECT totp_enabled, totp_secret FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?)),
        )?)
    })
}

/// Verifies a TOTP code OR a backup code against a user's factor.
/// Returns Ok(()) on success, AppError::unauthorized otherwise.
pub async fn verify_factor(
    state: &AppState,
    user_id: &str,
    code: &str,
) -> AppResult<()> {
    let (enabled, secret) = user_factor(state, user_id)?;
    if !enabled {
        return Err(AppError::forbidden("2fa not enabled"));
    }
    let secret = secret.ok_or_else(|| AppError::internal("totp enabled but secret missing"))?;
    if totp_matches(&secret, code) {
        return Ok(());
    }
    if consume_backup(state, user_id, code).await? {
        return Ok(());
    }
    Err(AppError::unauthorized("invalid totp or backup code"))
}

/// Consume a backup code if present + unused. Idempotent within a request:
/// if the row exists, it gets `used_at = now()` exactly once.
async fn consume_backup(
    state: &AppState,
    user_id: &str,
    code: &str,
) -> AppResult<bool> {
    let h = sha256_hex_str(code.trim().to_lowercase().as_bytes());
    let uid = user_id.to_string();
    Ok(state.db.writer.call(move |conn| {
        let updated = conn.execute(
            "UPDATE user_backup_codes SET used_at = ?1
             WHERE user_id = ?2 AND code_hash = ?3 AND used_at IS NULL",
            rusqlite::params![now(), uid, h],
        )?;
        Ok(updated > 0)
    }).await?)
}

// ---------- pure crypto: TOTP (RFC 6238) over HMAC-SHA1 ----------

/// Returns true if `code` equals the current TOTP value for `secret_b32`, OR the
/// previous/next 30s window (to absorb ±1 step clock drift, per RFC 6238 §5.2).
pub fn totp_matches(secret_b32: &str, code: &str) -> bool {
    let Some(secret) = base32::decode(ALPHABET, secret_b32) else { return false };
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let step = t / PERIOD;
    let candidate = code.trim().replace(' ', "");
    if candidate.len() != DIGITS as usize || !candidate.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    for s in [step.wrapping_sub(1), step, step.wrapping_add(1)] {
        if hotp(&secret, s) == candidate {
            return true;
        }
    }
    false
}

/// HOTP (RFC 4226): truncate HMAC-SHA1(secret, counter) to `DIGITS` decimal digits.
fn hotp(secret: &[u8], counter: u64) -> String {
    let mut mac = HmacSha1::new_from_slice(secret).expect("hmac key");
    mac.update(&counter.to_be_bytes());
    let bytes = mac.finalize().into_bytes();
    let offset = (bytes[bytes.len() - 1] & 0x0f) as usize;
    let bin = ((bytes[offset] as u32 & 0x7f) << 24)
        | ((bytes[offset + 1] as u32) << 16)
        | ((bytes[offset + 2] as u32) << 8)
        | (bytes[offset + 3] as u32);
    format!("{:0width$}", bin % 10u32.pow(DIGITS), width = DIGITS as usize)
}

fn sha256_hex_str(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    let out = h.finalize();
    out.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Minimal URL-encoding (RFC 3986 unreserved). Authenticator apps are picky.
fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector from RFC 6238 §Appendix B (key = "12345678901234567890" ASCII,
    // digits=6, algo=SHA1). We re-encode the ASCII to base32 to match the
    // function signature.
    const SECRET_ASCII: &[u8] = b"12345678901234567890";

    fn b32_of_ascii(s: &[u8]) -> String {
        b32(ALPHABET, s)
    }

    fn at(t: u64) -> String {
        let secret = SECRET_ASCII.to_vec();
        // Bypass the ±1 step window for the test.
        let mut mac = HmacSha1::new_from_slice(&secret).unwrap();
        let counter = t / PERIOD;
        mac.update(&counter.to_be_bytes());
        let bytes = mac.finalize().into_bytes();
        let offset = (bytes[bytes.len() - 1] & 0x0f) as usize;
        let bin = ((bytes[offset] as u32 & 0x7f) << 24)
            | ((bytes[offset + 1] as u32) << 16)
            | ((bytes[offset + 2] as u32) << 8)
            | (bytes[offset + 3] as u32);
        format!("{:06}", bin % 1_000_000)
    }

    #[test]
    fn rfc6238_known_values() {
        // t=59 → 94287082 truncated to 6 digits = 287082
        assert_eq!(at(59), "287082");
        // t=1111111109 → 08180427 truncated to 6 = 081804
        assert_eq!(at(1_111_111_109), "081804");
    }

    #[test]
    fn matches_round_trip() {
        let secret = b32_of_ascii(SECRET_ASCII);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let code = at(now);
        assert!(totp_matches(&secret, &code));
        assert!(!totp_matches(&secret, "000000"));
        // Padded-with-spaces variants are common user input.
        assert!(totp_matches(&secret, &format!(" {} ", code)));
    }
}
