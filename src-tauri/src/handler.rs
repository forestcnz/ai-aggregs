//! 请求处理入口：鉴权、model 路由、协议判定、透传/转换、回吐响应

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::{AppState, Protocol};
use crate::converter;
use crate::error::AppError;
use crate::stream;

/// 主代理入口：同时承载三个端点
pub async fn proxy(
    State(st): State<AppState>,
    req: Request,
) -> Result<Response, AppError> {
    // 1. 鉴权
    auth(&st, req.headers())?;

    // 2. consumer 协议由请求路径自动判定（三种端点均可访问）
    let c_proto = proto_from_path(req.uri().path());

    // 3. 解析 body，取 model
    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .map_err(|e| AppError::BadRequest(format!("read body: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes)?;
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| AppError::BadRequest("missing model".into()))?
        .to_string();

    // 4. model -> 候选 provider 列表（按配置顺序，第一个优先，后续为 failover 候选）
    let candidates = st
        .route(&model)
        .ok_or_else(|| AppError::ModelNotFound(model.clone()))?;
    let stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(false);

    // 5. 遍历候选 provider：每个 provider 内部已有 key 级 failover，
    //    外层再做 provider 级 failover。4xx(非429)视为请求本身问题，不切 provider。
    let mut last_err: Option<AppError> = None;
    for provider in &candidates {
        let p_proto = provider.protocol;
        // 协议判定 + 请求体转换（per-provider，因为不同 provider 协议可能不同）
        let (send_body, need_convert) = if c_proto == p_proto {
            (body.clone(), false)
        } else {
            match converter::req_convert(&body, c_proto, p_proto) {
                Ok(v) => (v, true),
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        };
        tracing::info!(
            model = %model,
            provider = %provider.name,
            consumer = ?c_proto,
            provider_proto = ?p_proto,
            stream,
            "proxy"
        );

        let endpoint = provider.endpoint();
        match provider.send(endpoint, &send_body, stream).await {
            Ok(resp) => {
                // send 成功 = 上游已开始返回，commit 到此 provider，不再 failover
                return build_response(resp, p_proto, c_proto, need_convert, stream).await;
            }
            Err(e) => {
                let status = e.status;
                last_err = Some(AppError::UpstreamStatus(status, e.message));
                // 4xx（非 429）是请求本身问题，换 provider 没用，直接返回
                if status >= 400 && status < 500 && status != 429 {
                    break;
                }
                // 429 / 5xx / 超时：切下一个 provider
                tracing::warn!(
                    provider = %provider.name,
                    model = %model,
                    status = status,
                    "provider failed, failover to next"
                );
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::Upstream("all providers exhausted".into())))
}

/// 把上游成功响应转换为客户端协议并返回
async fn build_response(
    resp: reqwest::Response,
    p_proto: Protocol,
    c_proto: Protocol,
    need_convert: bool,
    stream: bool,
) -> Result<Response, AppError> {
    if stream {
        if need_convert {
            Ok(stream::stream_convert(resp, p_proto, c_proto).await?)
        } else {
            Ok(stream::stream_passthrough(resp))
        }
    } else {
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?;
        let val: Value = serde_json::from_str(&text)
            .map_err(|e| AppError::Upstream(format!("bad upstream json: {e}; body={text}")))?;
        let out = if need_convert {
            converter::resp_convert(&val, p_proto, c_proto)?
        } else {
            val
        };
        Ok((status, Json(out)).into_response())
    }
}

/// 列出对外模型（兼容 OpenAI /v1/models 格式）
pub async fn list_models(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": st.consumer.models.iter().map(|m| json!({
            "id": m, "object": "model"
        })).collect::<Vec<_>>()
    }))
}

/// 根据请求路径判定 consumer 协议
fn proto_from_path(path: &str) -> Protocol {
    if path.ends_with("/responses") {
        Protocol::Responses
    } else if path.ends_with("/messages") {
        Protocol::Anthropic
    } else {
        Protocol::Chat
    }
}

/// 鉴权：支持 Authorization: Bearer xxx 或 x-api-key: xxx
fn auth(st: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let xkey = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    let presented = bearer.or(xkey).unwrap_or("");
    if st.consumer.check_key(presented) {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}
