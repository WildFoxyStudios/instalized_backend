//! Test harness: hermetic app instance per test (own temp SQLite, ephemeral port,
//! fast batch/sweep/pin intervals).

use backend_rust::config::Config;
use backend_rust::state::AppState;
use serde_json::Value;

// Each integration binary uses a subset of this harness — silence cross-target
// dead-code noise.
#[allow(dead_code)]
pub struct TestApp {
    pub base: String,
    pub http: reqwest::Client,
    pub state: AppState,
}

pub async fn spawn(modify: impl FnOnce(&mut Config)) -> TestApp {
    let dir = std::env::temp_dir().join(format!("bend-test-{}", backend_rust::db::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config {
        port: 0,
        database_path: dir.join("app.db").to_string_lossy().to_string(),
        jwt_secret: "test-secret".into(),
        web_base_url: "http://web.test".into(),
        gateways: vec!["https://ipfs.io".into()],
        pinning_api_url: None,
        pinning_token: String::new(),
        google_client_id: None,
        pinata_upload_jwt: None,
        batch_flush_ms: 50,
        story_sweep_secs: 1,
        pin_worker_secs: 1,
        access_ttl_secs: 900,
        refresh_ttl_secs: 3600,
        auth_rate_per_min: 1000, // generous default so unrelated tests never trip it
        fcm_service_account_path: None,
        fcm_project_id: None,
        push_worker_secs: 1,
    };
    modify(&mut cfg);
    let (app, state) = backend_rust::build(cfg).unwrap();
    backend_rust::spawn_workers(&state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    TestApp {
        base: format!("http://{addr}"),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        state,
    }
}

#[allow(dead_code)]
impl TestApp {
    pub fn ws_url(&self) -> String {
        format!("ws{}/v1/ws", self.base.trim_start_matches("http"))
    }

    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.expect("request");
        let status = resp.status().as_u16();
        let value = resp.json::<Value>().await.unwrap_or(Value::Null);
        (status, value)
    }

    pub async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.request(reqwest::Method::POST, path, token, Some(body)).await
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> (u16, Value) {
        self.request(reqwest::Method::GET, path, token, None).await
    }

    pub async fn put(&self, path: &str, token: Option<&str>) -> (u16, Value) {
        self.request(reqwest::Method::PUT, path, token, None).await
    }

    pub async fn put_with_body(
        &self,
        path: &str,
        token: Option<&str>,
        body: Value,
    ) -> (u16, Value) {
        self.request(reqwest::Method::PUT, path, token, Some(body)).await
    }

    pub async fn delete(&self, path: &str, token: Option<&str>) -> (u16, Value) {
        self.request(reqwest::Method::DELETE, path, token, None).await
    }

    /// Register a user; returns (access_token, refresh_token, user_id).
    pub async fn register(&self, name: &str) -> (String, String, String) {
        let (status, v) = self
            .post(
                "/v1/auth/register",
                None,
                serde_json::json!({
                    "email": format!("{name}@example.com"),
                    "username": name,
                    "password": "password123",
                }),
            )
            .await;
        assert_eq!(status, 200, "register {name}: {v}");
        (
            v["access_token"].as_str().unwrap().to_string(),
            v["refresh_token"].as_str().unwrap().to_string(),
            v["user_id"].as_str().unwrap().to_string(),
        )
    }

    /// Create a post for the token's user; returns the post id.
    pub async fn create_post(&self, token: &str, kind: &str, cid: &str) -> String {
        let (status, v) = self
            .post(
                "/v1/posts",
                Some(token),
                serde_json::json!({ "kind": kind, "media_cid": cid }),
            )
            .await;
        assert_eq!(status, 200, "create_post: {v}");
        v["id"].as_str().unwrap().to_string()
    }
}
