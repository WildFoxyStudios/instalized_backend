//! backend-rust — central coordination server (metadata + CIDs only).
//! Library form so integration tests can build the full app in-process.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod pinning;
pub mod seo;
pub mod state;
pub mod ws;

use crate::auth::google::GoogleVerifier;
use crate::config::Config;
use crate::error::AppResult;
use crate::pinning::client::PinningClient;
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
    let state = AppState {
        cfg: Arc::new(cfg),
        db,
        hub: Arc::new(Hub::default()),
        google: Arc::new(google),
        pinner: Arc::new(pinner),
    };
    Ok((api::router(state.clone()), state))
}

/// Start background workers: story sweeper + pin worker.
pub fn spawn_workers(state: &AppState) {
    api::stories::spawn_sweeper(state.clone());
    pinning::worker::spawn(state.clone());
}
