//! Minimal IETF Pinning Service API client (`POST {base}/pins`).
//! Provider-agnostic by design — swapping Pinata for Filebase is a config change.

use serde_json::json;

pub struct PinningClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl PinningClient {
    pub fn new(base: String, token: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base,
            token,
        }
    }

    /// Request the service to pin `cid`. Errors are returned as strings for the
    /// worker's retry bookkeeping.
    pub async fn pin_by_cid(&self, cid: &str, name: &str) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/pins", self.base))
            .bearer_auth(&self.token)
            .json(&json!({ "cid": cid, "name": name }))
            .timeout(std::time::Duration::from_secs(20))
            .send()
            .await
            .map_err(|e| format!("request: {e}"))?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("status {}", resp.status()))
        }
    }
}
