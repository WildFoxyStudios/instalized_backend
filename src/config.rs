//! Environment-driven configuration. Every knob is overridable in tests.

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub database_path: String,
    pub jwt_secret: String,
    /// Base URL of the SvelteKit web app (SEO redirects, CORS origin).
    pub web_base_url: String,
    /// Ordered public IPFS gateways used to build og:image URLs.
    pub gateways: Vec<String>,
    /// IETF Pinning Service API base (e.g. https://api.pinata.cloud/psa). None = worker idle.
    pub pinning_api_url: Option<String>,
    pub pinning_token: String,
    pub google_client_id: Option<String>,
    /// Pre-scoped, upload-only Pinata JWT handed to web clients (browser uploads
    /// go browser→Pinata directly; media bytes never touch this server).
    pub pinata_upload_jwt: Option<String>,
    /// Counter-batching flush interval (manifesto #6).
    pub batch_flush_ms: u64,
    pub story_sweep_secs: u64,
    pub pin_worker_secs: u64,
    pub access_ttl_secs: i64,
    pub refresh_ttl_secs: i64,
    /// Auth-surface rate limit (per IP per minute) — spec §15.
    pub auth_rate_per_min: u32,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| {
            tracing::warn!("JWT_SECRET not set — using insecure dev secret");
            "dev-secret-do-not-use-in-prod".to_string()
        });
        Self {
            port: env_or("PORT", "8080").parse().unwrap_or(8080),
            database_path: env_or("DATABASE_PATH", "./data/app.db"),
            jwt_secret,
            web_base_url: env_or("WEB_BASE_URL", "http://localhost:5173"),
            gateways: env_or(
                "GATEWAYS",
                "https://ipfs.io,https://cloudflare-ipfs.com,https://gateway.pinata.cloud",
            )
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect(),
            pinning_api_url: std::env::var("PINNING_API_URL")
                .ok()
                .map(|s| s.trim_end_matches('/').to_string()),
            pinning_token: env_or("PINNING_TOKEN", ""),
            google_client_id: std::env::var("GOOGLE_CLIENT_ID").ok(),
            pinata_upload_jwt: std::env::var("PINATA_UPLOAD_JWT").ok(),
            batch_flush_ms: env_or("BATCH_FLUSH_MS", "200").parse().unwrap_or(200),
            story_sweep_secs: env_or("STORY_SWEEP_SECS", "600").parse().unwrap_or(600),
            pin_worker_secs: env_or("PIN_WORKER_SECS", "5").parse().unwrap_or(5),
            access_ttl_secs: 15 * 60,
            refresh_ttl_secs: 30 * 24 * 3600,
            auth_rate_per_min: env_or("AUTH_RATE_PER_MIN", "10").parse().unwrap_or(10),
        }
    }

    /// Primary gateway URL for a CID (SEO og: tags).
    pub fn gateway_url(&self, cid: &str) -> String {
        let gw = self
            .gateways
            .first()
            .map(String::as_str)
            .unwrap_or("https://ipfs.io");
        format!("{gw}/ipfs/{cid}")
    }
}
