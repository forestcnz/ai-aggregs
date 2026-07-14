use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::state::AppState;
use crate::config::types::Protocol;
use crate::gateway::converter;
use crate::gateway::stream;
use crate::infra::error::AppError;

pub async fn proxy(State(st): State<AppState>, req: Request) -> Result<Response, AppError> {
    auth(&st, req.headers())?;

    let c_proto = proto_from_path(req.uri().path());
    let req_method = req.method().clone();
    let req_path = req.uri().path().to_string();

    let bytes = axum::body::to_bytes(req.into_body(), 64 * 1024 * 1024)
        .await
        .map_err(|e| AppError::BadRequest(format!("read body: {e}")))?;
    let body: Value = serde_json::from_slice(&bytes)?;
    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| AppError::BadRequest("missing model".into()))?
        .to_string();
    tracing::debug!(
        method = %req_method,
        path = %req_path,
        model = %model,
        consumer_proto = ?c_proto,
        body_len = bytes.len(),
        stream = %body.get("stream").map(|v| v.to_string()).unwrap_or_default(),
        "proxy: request entry"
    );

    let candidates = st
        .route(&model)
        .ok_or_else(|| AppError::ModelNotFound(model.clone()))?;
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    tracing::debug!(
        model = %model,
        candidates = ?candidates.iter().map(|p| &p.name).collect::<Vec<_>>(),
        stream,
        "proxy: route resolved"
    );

    let mut last_err: Option<AppError> = None;
    for provider in &candidates {
        let p_proto = provider.protocol;
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
        tracing::debug!(
            provider = %provider.name,
            protocol = ?p_proto,
            endpoint = %endpoint,
            need_convert,
            body_preview = %truncate_json(&send_body, 500),
            "proxy: sending to provider"
        );
        match provider.send(endpoint, &send_body, stream).await {
            Ok(resp) => {
                tracing::debug!(
                    provider = %provider.name,
                    status = %resp.status(),
                    "proxy: upstream responded"
                );
                return build_response(resp, p_proto, c_proto, need_convert, stream).await;
            }
            Err(e) => {
                let status = e.status;
                tracing::error!(
                    provider = %provider.name,
                    status = status,
                    message = %e.message,
                    "proxy: upstream send failed"
                );
                last_err = Some(AppError::UpstreamStatus(status, e.message));
                if status >= 400 && status < 500 && status != 429 {
                    break;
                }
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

async fn build_response(
    resp: reqwest::Response,
    p_proto: Protocol,
    c_proto: Protocol,
    need_convert: bool,
    stream: bool,
) -> Result<Response, AppError> {
    tracing::debug!(
        p_proto = ?p_proto,
        c_proto = ?c_proto,
        need_convert,
        stream,
        status = %resp.status(),
        "build_response"
    );
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

pub async fn list_models(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "object": "list",
        "data": st.consumer.models.iter().map(|m| json!({
            "id": m, "object": "model"
        })).collect::<Vec<_>>()
    }))
}

fn proto_from_path(path: &str) -> Protocol {
    if path.ends_with("/responses") {
        Protocol::Responses
    } else if path.ends_with("/messages") {
        Protocol::Anthropic
    } else {
        Protocol::Chat
    }
}

fn truncate_json(v: &Value, max: usize) -> String {
    let s = serde_json::to_string(v).unwrap_or_default();
    if s.len() > max {
        format!("{}... (truncated {})", &s[..max], s.len())
    } else {
        s
    }
}

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
