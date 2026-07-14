//! 错误类型与映射：HTTP 响应 (`AppError`) + Tauri IPC (`IpcError`)

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;

// ===================== HTTP 网关错误 =====================

/// 网关 HTTP 错误枚举（Axum 响应映射）
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
        (
            code,
            Json(serde_json::json!({
                "error": { "message": msg, "type": code.as_str() }
            })),
        )
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

// ===================== Tauri IPC 错误 =====================

/// Tauri IPC 错误：序列化为字符串发送到前端
///
/// 所有 `#[tauri::command]` 的 `Result<T, E>` 中 `E` 使用此类型，
/// 替代裸 `String`，消除 `.map_err(|e| e.to_string())` 样板代码。
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

/// 自动从 `anyhow::Error` 转换（覆盖 db / provider / config 错误）
impl From<anyhow::Error> for IpcError {
    fn from(e: anyhow::Error) -> Self {
        IpcError(e.to_string())
    }
}

/// 自动从 `String` 转换（覆盖已格式化的错误消息）
impl From<String> for IpcError {
    fn from(s: String) -> Self {
        IpcError(s)
    }
}
