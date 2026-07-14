//! 错误类型与 HTTP 响应映射

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use thiserror::Error;

/// 应用错误枚举
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
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (code, msg) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, self.to_string()),
            AppError::ModelNotFound(_) => (StatusCode::NOT_FOUND, self.to_string()),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
            AppError::UpstreamStatus(s, _) => {
                // 透传上游原始状态码（如 429 限流/余额不足），无法解析时退回 502
                let code = StatusCode::from_u16(*s).unwrap_or(StatusCode::BAD_GATEWAY);
                (code, self.to_string())
            }
            AppError::Other(_) => (StatusCode::INTERNAL_SERVER_ERROR, self.to_string()),
        };
        // 统一返回 OpenAI 风格错误体，方便各类客户端解析
        (code, Json(serde_json::json!({
            "error": { "message": msg, "type": code.as_str() }
        })))
            .into_response()
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::BadRequest(format!("invalid json: {e}"))
    }
}
