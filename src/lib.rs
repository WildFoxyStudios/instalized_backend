//! backend-rust — central coordination server (metadata + CIDs only).
//! Library form so integration tests can build the full app in-process.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod pinning;
pub mod push;
pub mod seo;
pub mod state;
pub mod ws;

use crate::auth::google::GoogleVerifier;
use crate::config::Config;
use crate::error::AppResult;
use crate::pinning::client::PinningClient;
use crate::push::fcm::FcmClient;
use crate::state::AppState;
use crate::ws::hub::Hub;
use std::sync::Arc;

/// Build the router + state. Must be called inside a tokio runtime
/// (the DB writer actor spawns immediately).
pub fn build(cfg: Config) -> AppResult<(axum::Router, AppState)> {
    let db = db::Db::open(&cfg)?;
    let google = cfg.google_client_id.clone().map(GoogleVerifier::new);
    let pinner = cfg
        .pinning_api_url
        .clone()
        .map(|url| PinningClient::new(url, cfg.pinning_token.clone()));
    let fcm = build_fcm(&cfg);
    let limiter = Arc::new(api::ratelimit::RateLimiter::per_minute(cfg.auth_rate_per_min));
    let state = AppState {
        cfg: Arc::new(cfg),
        db,
        hub: Arc::new(Hub::default()),
        google: Arc::new(google),
        pinner: Arc::new(pinner),
        fcm: Arc::new(fcm),
        limiter,
    };
    Ok((api::router(state.clone()), state))
}

fn build_fcm(cfg: &Config) -> Option<FcmClient> {
    let path = cfg.fcm_service_account_path.as_deref()?;
    match FcmClient::from_service_account_file(path, cfg.fcm_project_id.clone()) {
        Ok(c) => {
            tracing::info!("push: FCM client loaded from {}", path);
            Some(c)
        }
        Err(e) => {
            tracing::error!("push: FCM init failed, worker will be disabled: {e}");
            None
        }
    }
}

/// Start background workers: story sweeper + pin worker + push worker.
pub fn spawn_workers(state: &AppState) {
    api::stories::spawn_sweeper(state.clone());
    pinning::worker::spawn(state.clone());
    push::worker::spawn(state.clone());
}
