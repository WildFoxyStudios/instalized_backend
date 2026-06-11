//! FCM HTTP v1 client: service-account key, RS256 JWT → OAuth2 access token,
//! POST /v1/projects/{project_id}/messages:send.

use crate::error::{AppError, AppResult};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Subset of the Google Cloud service-account JSON key we need.
#[derive(Clone, Debug, Deserialize)]
struct ServiceAccountKey {
    client_email: String,
    /// PEM-encoded RSA private key. Comes with literal `\n` separators in
    /// the JSON; we fix them up at parse time.
    private_key: String,
    /// Sometimes the service-account JSON includes `project_id` so we can
    /// read it from there; in production it's also set via FCM_PROJECT_ID.
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64, // seconds
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

/// Long-lived FCM client. Cloning it is cheap (inner is `Arc`'d).
#[derive(Clone)]
pub struct FcmClient {
    inner: Arc<FcmInner>,
}

struct FcmInner {
    project_id: String,
    key: EncodingKey,
    client_email: String,
    token_endpoint: String,
    fcm_endpoint: String,
    http: reqwest::Client,
    cached: Mutex<Option<CachedToken>>,
}

impl FcmClient {
    /// Load a service-account JSON file and build a client.
    pub fn from_service_account_file(
        path: &str,
        project_id_override: Option<String>,
    ) -> AppResult<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AppError::internal(format!("FCM service account read failed: {e}"))
        })?;
        Self::from_service_account_json(&raw, project_id_override)
    }

    pub fn from_service_account_json(
        json: &str,
        project_id_override: Option<String>,
    ) -> AppResult<Self> {
        let key: ServiceAccountKey = serde_json::from_str(json).map_err(|e| {
            AppError::internal(format!("FCM service account parse: {e}"))
        })?;
        // The private key string in the JSON is PKCS#8 PEM, with `\n` as
        // literal two-character escapes. Unescape them so jsonwebtoken's
        // PKCS#8 loader can read it.
        let pem = key.private_key.replace("\\n", "\n");
        let encoding = EncodingKey::from_rsa_pem(pem.as_bytes()).map_err(|e| {
            AppError::internal(format!("FCM private key parse: {e}"))
        })?;
        let project_id = project_id_override
            .or(key.project_id.clone())
            .ok_or_else(|| AppError::internal("FCM project_id missing"))?;
        Ok(Self {
            inner: Arc::new(FcmInner {
                project_id,
                key: encoding,
                client_email: key.client_email,
                token_endpoint: "https://oauth2.googleapis.com/token".into(),
                fcm_endpoint: "https://fcm.googleapis.com/v1/projects".into(),
                http: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .map_err(|e| AppError::internal(format!("reqwest: {e}")))?,
                cached: Mutex::new(None),
            }),
        })
    }

    /// Send a single FCM data message to a token. Returns Ok(()) on a 200,
    /// Err with the FCM error code on a 4xx/5xx.
    pub async fn send_to_token(
        &self,
        token: &str,
        title: &str,
        body: &str,
        data: &[(String, String)],
    ) -> AppResult<()> {
        let access = self.get_access_token().await?;
        let url = format!(
            "{}/{}/messages:send",
            self.inner.fcm_endpoint, self.inner.project_id
        );
        let mut data_obj = serde_json::Map::new();
        for (k, v) in data {
            data_obj.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        let payload = serde_json::json!({
            "message": {
                "token": token,
                "notification": { "title": title, "body": body },
                "data": data_obj,
                "android": { "priority": "HIGH" },
            }
        });
        let resp = self
            .inner
            .http
            .post(&url)
            .bearer_auth(&access)
            .header("content-type", "application/json; charset=utf-8")
            .json(&payload)
            .send()
            .await
            .map_err(|e| AppError::internal(format!("fcm send: {e}")))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }
        // Body is small JSON like { "error": { "code": 404, "message": "...", "status": "NOT_FOUND" } }
        let body_text = resp.text().await.unwrap_or_default();
        let code = serde_json::from_str::<serde_json::Value>(&body_text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("status"))
                    .and_then(|s| s.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| "UNKNOWN".into());
        Err(AppError::internal(format!(
            "fcm http {} status={} body={}",
            status.as_u16(),
            code,
            truncate(&body_text, 200)
        )))
    }

    /// Returns true if the FCM error code indicates the token is permanently
    /// dead and should be removed from the next fan-out. From the FCM docs
    /// the canonical "unregister" codes are:
    ///   * UNREGISTERED — app uninstalled or token expired.
    ///   * INVALID_ARGUMENT — malformed token (programming error).
    ///   * SENDER_ID_MISMATCH — token was issued to a different project.
    ///   * THIRD_PARTY_AUTH_ERROR — APNs key issue, iOS-only.
    pub fn is_dead_token(code: &str) -> bool {
        matches!(
            code,
            "UNREGISTERED"
                | "INVALID_ARGUMENT"
                | "SENDER_ID_MISMATCH"
                | "THIRD_PARTY_AUTH_ERROR"
        )
    }

    async fn get_access_token(&self) -> AppResult<String> {
        // 1. Reuse if still fresh (5-minute safety margin).
        if let Some(c) = self.inner.cached.lock().unwrap().as_ref() {
            if c.expires_at > Instant::now() + std::time::Duration::from_secs(300) {
                return Ok(c.token.clone());
            }
        }
        // 2. Build a self-signed JWT asserting the FCM scope.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let claims = serde_json::json!({
            "iss": self.inner.client_email,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "aud": self.inner.token_endpoint,
            "iat": now,
            "exp": now + 3600, // max is 1h; we cache for ~55m
        });
        let jwt = encode(
            &Header::new(Algorithm::RS256),
            &claims,
            &self.inner.key,
        )
        .map_err(|e| AppError::internal(format!("fcm jwt sign: {e}")))?;
        // 3. Exchange at the token endpoint.
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &jwt),
        ];
        let resp = self
            .inner
            .http
            .post(&self.inner.token_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await
            .map_err(|e| AppError::internal(format!("fcm token fetch: {e}")))?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::internal(format!(
                "fcm token http failed: {}",
                truncate(&body, 200)
            )));
        }
        let parsed: TokenResponse = resp
            .json()
            .await
            .map_err(|e| AppError::internal(format!("fcm token parse: {e}")))?;
        let expires_at = Instant::now()
            + std::time::Duration::from_secs(parsed.expires_in.saturating_sub(60));
        *self.inner.cached.lock().unwrap() = Some(CachedToken {
            token: parsed.access_token.clone(),
            expires_at,
        });
        Ok(parsed.access_token)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char-boundary at or below `max`.
        let mut i = max;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        &s[..i]
    }
}
