use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("upstream {0}: {1}")]
    UpstreamStatus(u16, String),
    #[error("bad request: {0}")]
    BadRequest(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (code, msg) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::ModelNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::UpstreamStatus(s, _) => {
                let code = StatusCode::from_u16(*s).unwrap_or(StatusCode::BAD_GATEWAY);
                (code, self.to_string())
            }
        };
        (
            code,
            Json(serde_json::json!({
                "error": { "message": msg, "type": code.as_str() }
            })),
        )
            .into_response()
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::BadRequest(format!("invalid json: {e}"))
    }
}

#[derive(Debug, Serialize)]
pub struct IpcError(pub String);

impl IpcError {
    pub fn new(msg: impl Into<String>) -> Self {
        IpcError(msg.into())
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IpcError {}

impl From<anyhow::Error> for IpcError {
    fn from(e: anyhow::Error) -> Self {
        IpcError(e.to_string())
    }
}
