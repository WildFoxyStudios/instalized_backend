//! Integration tests for the 2FA / TOTP endpoint surface:
//!   POST /v1/users/me/2fa/enable   → {secret, otpauth_uri, backup_codes}
//!   POST /v1/users/me/2fa/confirm  → activate
//!   POST /v1/auth/login            → returns requires_2fa + pending_token
//!   POST /v1/auth/2fa              → exchanges pending_token + code for tokens
//!   POST /v1/users/me/2fa/disable  → requires password + (TOTP or backup code)

mod common;

use hmac::{Hmac, Mac};
use rand::RngCore;
use serde_json::json;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha1 = Hmac<Sha1>;

const PERIOD: u64 = 30;
const DIGITS: u32 = 6;
const ALPHABET: base32::Alphabet = base32::Alphabet::Rfc4648 { padding: false };

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

fn current_totp(secret_b32: &str) -> String {
    let secret = base32::decode(ALPHABET, secret_b32).expect("decode b32");
    let t = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    hotp(&secret, t / PERIOD)
}

fn current_totp_for_step(secret_b32: &str, step: u64) -> String {
    let secret = base32::decode(ALPHABET, secret_b32).expect("decode b32");
    hotp(&secret, step)
}

#[tokio::test]
async fn status_defaults_off_for_new_user() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("alice").await;
    let (status, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(status, 200);
    assert_eq!(v["totp_enabled"], false);
    assert_eq!(v["backup_codes_remaining"], 0);
}

#[tokio::test]
async fn enable_returns_secret_and_backup_codes() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("alice").await;
    let (status, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    assert_eq!(status, 200, "enable: {v}");
    let secret = v["secret"].as_str().unwrap();
    // 20 bytes → 32 chars of base32 (no padding).
    assert_eq!(secret.len(), 32);
    assert!(v["otpauth_uri"].as_str().unwrap().starts_with("otpauth://totp/"));
    let codes = v["backup_codes"].as_array().unwrap();
    assert_eq!(codes.len(), 10);
    for c in codes {
        let s = c.as_str().unwrap();
        assert_eq!(s.len(), 11); // 5 + '-' + 5
        assert!(s.chars().nth(5) == Some('-'));
    }
    // totp_enabled must still be false until confirm.
    let (_, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(v["totp_enabled"], false);
    assert_eq!(v["backup_codes_remaining"], 10);
}

#[tokio::test]
async fn confirm_with_wrong_code_keeps_disabled() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("alice").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    // Compute a code from a *previous* 30s window so the lookup misses.
    let step = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() / PERIOD - 5;
    let bad = current_totp_for_step(&secret, step);
    let (status, _) = app
        .post("/v1/users/me/2fa/confirm", Some(&token), json!({ "code": bad }))
        .await;
    // 200 is returned for invalid input? Server returns 400 from `AppError::bad_request`.
    assert_eq!(status, 400);
    let (_, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(v["totp_enabled"], false);
}

#[tokio::test]
async fn confirm_with_correct_code_enables_2fa() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("alice").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let code = current_totp(&secret);
    let (status, _) = app
        .post("/v1/users/me/2fa/confirm", Some(&token), json!({ "code": code }))
        .await;
    assert_eq!(status, 200);
    let (_, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(v["totp_enabled"], true);
}

#[tokio::test]
async fn login_2fa_pending_flow() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("bob").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let code = current_totp(&secret);
    app.post("/v1/users/me/2fa/confirm", Some(&token), json!({ "code": code }))
        .await;

    // Login with correct password → no access token, just a pending token.
    let (status, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "bob@example.com", "password": "password123" }),
        )
        .await;
    assert_eq!(status, 200);
    assert_eq!(v["requires_2fa"], true);
    let pending = v["pending_token"].as_str().unwrap();
    assert!(v["access_token"].is_null());

    // Wrong code → 401.
    let (status, _) = app
        .post(
            "/v1/auth/2fa",
            None,
            json!({ "pending_token": pending, "code": "000000" }),
        )
        .await;
    assert_eq!(status, 401);

    // Correct code → real access + refresh tokens.
    let (status, v) = app
        .post(
            "/v1/auth/2fa",
            None,
            json!({ "pending_token": pending, "code": current_totp(&secret) }),
        )
        .await;
    assert_eq!(status, 200, "verify_2fa: {v}");
    assert!(v["access_token"].as_str().is_some());
    assert!(v["refresh_token"].as_str().is_some());
}

#[tokio::test]
async fn pending_token_cannot_be_used_as_access_token() {
    // A user enables 2FA, attempts to use the pending token as if it were an
    // access token to hit /v1/users/me. The AuthUser extractor should reject it
    // because the sub has the "2fa:pending:" prefix (no row matches).
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("carol").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    app.post(
        "/v1/users/me/2fa/confirm",
        Some(&token),
        json!({ "code": current_totp(&secret) }),
    )
    .await;
    let (_, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "carol@example.com", "password": "password123" }),
        )
        .await;
    let pending = v["pending_token"].as_str().unwrap();
    // Pending token is signed like a JWT but `2fa:pending:<uid>` isn't a user id
    // — AuthUser::from_request will look up the row and fail.
    let (status, _) = app.get("/v1/users/me", Some(pending)).await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn login_with_2fa_off_keeps_legacy_path() {
    // Regression: a user without 2FA still gets the full pair directly.
    let app = common::spawn(|_| {}).await;
    let _ = app.register("dave").await;
    let (status, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "dave@example.com", "password": "password123" }),
        )
        .await;
    assert_eq!(status, 200);
    assert!(v["access_token"].as_str().is_some());
    assert!(v["requires_2fa"].is_null());
}

#[tokio::test]
async fn backup_code_unlocks_login_and_consumes() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("eve").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let codes: Vec<String> = v["backup_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    app.post(
        "/v1/users/me/2fa/confirm",
        Some(&token),
        json!({ "code": current_totp(&secret) }),
    )
    .await;

    // Login → pending token. Use a backup code.
    let (_, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "eve@example.com", "password": "password123" }),
        )
        .await;
    let pending = v["pending_token"].as_str().unwrap().to_string();
    let (status, _) = app
        .post(
            "/v1/auth/2fa",
            None,
            json!({ "pending_token": pending, "code": codes[0] }),
        )
        .await;
    assert_eq!(status, 200, "backup code should unlock");

    // Login again, try the *same* code → 401 (one-shot).
    let (_, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "eve@example.com", "password": "password123" }),
        )
        .await;
    let pending = v["pending_token"].as_str().unwrap().to_string();
    let (status, _) = app
        .post(
            "/v1/auth/2fa",
            None,
            json!({ "pending_token": pending, "code": codes[0] }),
        )
        .await;
    assert_eq!(status, 401, "reusing a consumed backup code must fail");

    // 9 codes remain.
    let (_, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(v["backup_codes_remaining"], 9);
}

#[tokio::test]
async fn disable_requires_password_and_factor() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("frank").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let code = current_totp(&secret);
    app.post("/v1/users/me/2fa/confirm", Some(&token), json!({ "code": code.clone() }))
        .await;

    // Wrong password → 401.
    let (status, _) = app
        .post(
            "/v1/users/me/2fa/disable",
            Some(&token),
            json!({ "password": "wrong", "code": code.clone() }),
        )
        .await;
    assert_eq!(status, 401);

    // Wrong code → 401.
    let (status, _) = app
        .post(
            "/v1/users/me/2fa/disable",
            Some(&token),
            json!({ "password": "password123", "code": "000000" }),
        )
        .await;
    assert_eq!(status, 401);

    // Correct password + TOTP → ok, totp_enabled flips to false.
    let (status, _) = app
        .post(
            "/v1/users/me/2fa/disable",
            Some(&token),
            json!({ "password": "password123", "code": current_totp(&secret) }),
        )
        .await;
    assert_eq!(status, 200);
    let (_, v) = app.get("/v1/users/me/2fa", Some(&token)).await;
    assert_eq!(v["totp_enabled"], false);
    assert_eq!(v["backup_codes_remaining"], 0);
}

#[tokio::test]
async fn enabling_again_invalidates_old_backup_codes() {
    let app = common::spawn(|_| {}).await;
    let (token, _, _) = app.register("gina").await;
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret = v["secret"].as_str().unwrap().to_string();
    let codes_first: Vec<String> = v["backup_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    app.post(
        "/v1/users/me/2fa/confirm",
        Some(&token),
        json!({ "code": current_totp(&secret) }),
    )
    .await;
    // Re-enable: we get a fresh set, the first set must no longer unlock login.
    let (_, v) = app.post("/v1/users/me/2fa/enable", Some(&token), json!({})).await;
    let secret2 = v["secret"].as_str().unwrap().to_string();
    let codes_second: Vec<String> = v["backup_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_ne!(codes_first[0], codes_second[0]);
    // Confirm the second enable so the user has 2fa on again.
    app.post(
        "/v1/users/me/2fa/confirm",
        Some(&token),
        json!({ "code": current_totp(&secret2) }),
    )
    .await;

    // Try a code from the *first* batch at /v1/auth/2fa → 401.
    let (_, v) = app
        .post(
            "/v1/auth/login",
            None,
            json!({ "email": "gina@example.com", "password": "password123" }),
        )
        .await;
    let pending = v["pending_token"].as_str().unwrap().to_string();
    let (status, _) = app
        .post(
            "/v1/auth/2fa",
            None,
            json!({ "pending_token": pending, "code": codes_first[0] }),
        )
        .await;
    assert_eq!(status, 401);
}

#[test]
fn sha256_hex_matches_a_known_vector() {
    // Sanity check: SHA-256 of "abc" = ba7816bf...f20015ad.
    let mut h = Sha256::new();
    h.update(b"abc");
    let out = h.finalize();
    let s: String = out.iter().map(|b| format!("{:02x}", b)).collect();
    assert_eq!(
        s,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn hotp_matches_rfc4226_vector() {
    // RFC 4226 Appendix D, secret = "12345678901234567890":
    //   counter=0 → 755224
    //   counter=1 → 287082
    let secret = b"12345678901234567890";
    assert_eq!(hotp(secret, 0), "755224");
    assert_eq!(hotp(secret, 1), "287082");
}

#[test]
fn base32_roundtrip_with_known_secret() {
    let raw = [7u8; 20];
    let s = base32::encode(ALPHABET, &raw);
    let decoded = base32::decode(ALPHABET, &s).unwrap();
    assert_eq!(decoded, raw);
}

// `rand::RngCore` is only used in this file when we need to generate a fake
// secret for negative tests. Keep the import to make that path obvious even
// though it's currently unused.
#[allow(dead_code)]
fn _silence_unused(_: &mut impl RngCore) {}
