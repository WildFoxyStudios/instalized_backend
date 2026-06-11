//! Google id_token verification (OAuth code+PKCE happens on the client; we verify
//! the resulting id_token against Google's JWKS, cached for 12 h).

use crate::error::{AppError, AppResult};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

const JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const JWKS_TTL: Duration = Duration::from_secs(12 * 3600);

#[derive(Debug, Deserialize, Clone)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize, Clone)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
pub struct GoogleClaims {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub picture: Option<String>,
}

pub struct GoogleVerifier {
    client_id: String,
    http: reqwest::Client,
    jwks: Arc<RwLock<Option<(Instant, Jwks)>>>,
}

impl GoogleVerifier {
    pub fn new(client_id: String) -> Self {
        Self {
            client_id,
            http: reqwest::Client::new(),
            jwks: Arc::new(RwLock::new(None)),
        }
    }

    async fn jwks(&self) -> AppResult<Jwks> {
        if let Some((at, jwks)) = self.jwks.read().await.as_ref() {
            if at.elapsed() < JWKS_TTL {
                return Ok(jwks.clone());
            }
        }
        let fresh: Jwks = self
            .http
            .get(JWKS_URL)
            .send()
            .await
            .map_err(|e| AppError::unavailable(format!("google jwks fetch: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::unavailable(format!("google jwks parse: {e}")))?;
        *self.jwks.write().await = Some((Instant::now(), fresh.clone()));
        Ok(fresh)
    }

    pub async fn verify(&self, id_token: &str) -> AppResult<GoogleClaims> {
        let header = decode_header(id_token)
            .map_err(|_| AppError::unauthorized("malformed google token"))?;
        let kid = header
            .kid
            .ok_or_else(|| AppError::unauthorized("google token missing kid"))?;
        let jwks = self.jwks().await?;
        let jwk = jwks
            .keys
            .iter()
            .find(|k| k.kid == kid)
            .ok_or_else(|| AppError::unauthorized("unknown google signing key"))?;
        let key = DecodingKey::from_rsa_components(&jwk.n, &jwk.e)
            .map_err(|e| AppError::internal(format!("jwk parse: {e}")))?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.client_id]);
        validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
        decode::<GoogleClaims>(id_token, &key, &validation)
            .map(|d| d.claims)
            .map_err(|_| AppError::unauthorized("invalid google token"))
    }
}
