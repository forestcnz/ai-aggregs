//! axum 路由表

use axum::routing::{get, post};
use axum::Router;

use crate::config::AppState;
use crate::handler;

pub fn build(state: AppState) -> Router {
    // 三种协议端点全部暴露，由 handler 根据请求路径自动判定 consumer 协议
    Router::new()
        // /v1/models 始终暴露
        .route("/v1/models", get(handler::list_models))
        // chat / responses / anthropic 三种协议均可正常访问
        .route("/v1/chat/completions", post(handler::proxy))
        .route("/v1/responses", post(handler::proxy))
        .route("/v1/messages", post(handler::proxy))
        .with_state(state)
}
