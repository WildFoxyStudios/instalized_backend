//! Unified error type. Every handler returns `AppResult<T>`; errors render as JSON.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{1}")]
    Status(StatusCode, String),
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("internal: {0}")]
    Internal(String),
}

impl AppError {
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::BAD_REQUEST, m.into())
    }
    pub fn unauthorized(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::UNAUTHORIZED, m.into())
    }
    pub fn forbidden(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::FORBIDDEN, m.into())
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::NOT_FOUND, m.into())
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::CONFLICT, m.into())
    }
    pub fn unavailable(m: impl Into<String>) -> Self {
        Self::Status(StatusCode::SERVICE_UNAVAILABLE, m.into())
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::Internal(m.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            AppError::Status(c, m) => (c, m),
            AppError::Db(rusqlite::Error::QueryReturnedNoRows) => {
                (StatusCode::NOT_FOUND, "not found".to_string())
            }
            AppError::Db(rusqlite::Error::SqliteFailure(e, m))
                if e.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                tracing::debug!("constraint violation: {m:?}");
                (StatusCode::CONFLICT, "already exists".to_string())
            }
            AppError::Db(e) => {
                tracing::error!("db error: {e}");
                (StatusCode::INTERNAL_SERVER_ERROR, "database error".to_string())
            }
            AppError::Internal(m) => {
                tracing::error!("internal error: {m}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
        };
        (code, Json(json!({ "error": msg }))).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;
