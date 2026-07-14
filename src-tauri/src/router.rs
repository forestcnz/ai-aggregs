//! axum 路由表

use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::Response;
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
        // CORS 中间件：允许 Tauri webview 前端直接 fetch 本地网关
        .layer(from_fn(cors_middleware))
        .with_state(state)
}

/// CORS 中间件：为所有响应注入跨域头，并直接应答 OPTIONS 预检请求
async fn cors_middleware(req: Request, next: Next) -> Response {
    // 预检请求直接返回 204，不带 body
    if req.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("access-control-allow-origin", "*")
            .header("access-control-allow-methods", "GET, POST, OPTIONS")
            .header("access-control-allow-headers", "*")
            .header("access-control-max-age", "86400")
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    headers.insert("access-control-allow-headers", HeaderValue::from_static("*"));
    headers.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, POST, OPTIONS"),
    );
    resp
}
