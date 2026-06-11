//! Shared application state (cheap to clone — everything is Arc'd).

use crate::auth::google::GoogleVerifier;
use crate::config::Config;
use crate::db::Db;
use crate::pinning::client::PinningClient;
use crate::ws::hub::Hub;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub hub: Arc<Hub>,
    pub google: Arc<Option<GoogleVerifier>>,
    pub pinner: Arc<Option<PinningClient>>,
}
