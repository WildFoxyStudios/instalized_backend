//! Per-IP token bucket guarding the auth surface (spec §15). Zero new deps:
//! a Mutex'd map with opportunistic stale-bucket cleanup. Behind Fly's proxy
//! the client IP arrives in `Fly-Client-IP`; sockets are the fallback.

use crate::state::AppState;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::Instant;

pub struct RateLimiter {
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub fn per_minute(rate: u32) -> Self {
        let rate = rate.max(1) as f64;
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: rate,
            refill_per_sec: rate / 60.0,
        }
    }

    /// Returns true when the request is allowed.
    pub fn check(&self, ip: IpAddr) -> bool {
        let mut buckets = self.buckets.lock().expect("limiter lock");
        let now = Instant::now();
        // Bound memory: drop buckets idle >10 min once the map grows large.
        if buckets.len() > 10_000 {
            buckets.retain(|_, b| now.duration_since(b.last).as_secs() < 600);
        }
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

fn header_ip(req: &Request, name: &str) -> Option<IpAddr> {
    req.headers()
        .get(name)?
        .to_str()
        .ok()?
        .split(',')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Proxy headers first (Fly), then the socket address.
fn client_ip(req: &Request) -> Option<IpAddr> {
    header_ip(req, "fly-client-ip")
        .or_else(|| header_ip(req, "x-forwarded-for"))
        .or_else(|| {
            req.extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip())
        })
}

pub async fn limit_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(ip) = client_ip(&req) else {
        return next.run(req).await; // no addressable client (in-process tests)
    };
    if state.limiter.check(ip) {
        next.run(req).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({ "error": "rate limited — retry later" })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_drains_and_refills() {
        let rl = RateLimiter::per_minute(3);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(rl.check(ip) && rl.check(ip) && rl.check(ip), "burst up to capacity");
        assert!(!rl.check(ip), "fourth call within the window is rejected");
        // Other IPs have independent buckets.
        assert!(rl.check("10.0.0.2".parse().unwrap()));
    }
}
