use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::api::handler;
use crate::config::state::AppState;

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/v1/models", get(handler::list_models))
        .route("/v1/chat/completions", post(handler::proxy))
        .route("/v1/responses", post(handler::proxy))
        .route("/v1/messages", post(handler::proxy))
        .layer(from_fn(cors_middleware))
        .with_state(state)
}

/// 判断请求的 Origin 是否为允许跨域调用网关的来源：
///   - 无 Origin / Referer：命令行/Postman 等非浏览器调用，放行（CORS 不限制）
///   - localhost / 127.0.0.1 任意端口：本地开发与桌面应用
///   - Tauri 自身的 webview origin：tauri.localhost / ipc.localhost
///   - 其他公网域名：禁止，避免任意网页跨域调用本地网关消耗用户额度
fn allowed_origin(origin: &str) -> bool {
    // 提取 host 部分（去掉 scheme）
    let host = if let Some(idx) = origin.find("://") {
        &origin[idx + 3..]
    } else {
        origin
    };
    let host = host.split('/').next().unwrap_or(host);
    // 去 port
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(
        host,
        "localhost" | "127.0.0.1" | "tauri.localhost" | "ipc.localhost"
    )
}

/// 从 Origin 或 Referer 头提取来源（取其一即可）
fn request_origin(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            headers
                .get("referer")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        })
}

async fn cors_middleware(req: Request, next: Next) -> Response {
    let origin = request_origin(req.headers());

    // OPTIONS 预检
    if req.method() == Method::OPTIONS {
        // 无 origin（非浏览器）→ 直接返回 NO_CONTENT
        // 有 origin → 仅白名单回显 ACAO，其余返回 403
        let resp_origin = match &origin {
            None => None,
            Some(o) if allowed_origin(o) => Some(o.clone()),
            Some(_) => {
                return Response::builder()
                    .status(StatusCode::FORBIDDEN)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
        };
        let mut builder = Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("access-control-allow-methods", "GET, POST, OPTIONS")
            .header("access-control-allow-headers", "*")
            .header("access-control-max-age", "86400");
        if let Some(o) = resp_origin {
            builder = builder.header("access-control-allow-origin", o);
        }
        return builder.body(axum::body::Body::empty()).unwrap();
    }

    let mut resp = next.run(req).await;
    let headers = resp.headers_mut();

    // 回显 ACAO：仅对允许的 origin 回显具体值（不使用 *）
    if let Some(o) = origin.filter(|o| allowed_origin(o)) {
        if let Ok(v) = HeaderValue::from_str(&o) {
            headers.insert("access-control-allow-origin", v);
        }
        headers.insert(
            "access-control-allow-headers",
            HeaderValue::from_static("*"),
        );
        headers.insert(
            "access-control-allow-methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::allowed_origin;

    #[test]
    fn allows_localhost_variants() {
        assert!(allowed_origin("http://localhost:1420"));
        assert!(allowed_origin("http://127.0.0.1:8849"));
        assert!(allowed_origin("https://tauri.localhost"));
        assert!(allowed_origin("http://ipc.localhost"));
    }

    #[test]
    fn rejects_public_origin() {
        assert!(!allowed_origin("https://evil.example.com"));
        assert!(!allowed_origin("http://attacker.net:8080"));
    }
}
