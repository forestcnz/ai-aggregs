use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use crate::config::state::AppState;
use crate::config::types::Protocol;
use crate::gateway::converter;
use crate::gateway::stream::{self, UsageCtx};
use crate::infra::error::AppError;

pub async fn proxy(State(st): State<AppState>, req: Request) -> Result<Response, AppError> {
    let consumer_key = auth(&st, req.headers())?;
    // 捕获 incoming 请求头快照（req.into_body 会消耗 req）
    let incoming_headers = req.headers().clone();

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
    // 日志策略：
    //   - debug：仅记录元信息 + 截断预览（500 字符），避免 PII/敏感内容长期落盘
    //   - trace：完整 body（仅极深度排查时才开启，且日志保留期 30 天）
    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            method = %req_method,
            path = %req_path,
            model = %model,
            consumer_proto = ?c_proto,
            stream = %body.get("stream").map(|v| v.to_string()).unwrap_or_default(),
            body_preview = %truncate_json(&body, 500),
            "← 下游请求"
        );
    }

    let candidates = st
        .route(&model, c_proto)
        .ok_or_else(|| AppError::ModelNotFound(model.clone()))?;
    let stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    tracing::debug!(
        model = %model,
        candidates = ?candidates.iter().map(|(p, m)| format!("{}→{}", p.name, m)).collect::<Vec<_>>(),
        stream,
        "proxy: route resolved"
    );

    let mut last_err: Option<AppError> = None;
    for (provider, actual_model) in &candidates {
        let p_proto = provider.protocol;
        // 别名重定向：把请求体里的 model 改写为实际后端模型
        let mut send_body = body.clone();
        if actual_model != &model {
            send_body["model"] = serde_json::Value::String(actual_model.clone());
        }
        let (mut send_body, need_convert) = if c_proto == p_proto {
            (send_body, false)
        } else {
            match converter::req_convert(&send_body, c_proto, p_proto) {
                Ok(v) => (v, true),
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            }
        };
        // 流式 Chat 请求注入 stream_options.include_usage，确保上游在末尾 chunk 返回 token 用量
        if stream && p_proto == Protocol::Chat {
            if let Some(obj) = send_body.as_object_mut() {
                let so = obj
                    .entry("stream_options")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if let Some(so_obj) = so.as_object_mut() {
                    so_obj.insert("include_usage".into(), Value::Bool(true));
                }
            }
        }
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
        match provider
            .send(endpoint, &send_body, stream, &incoming_headers)
            .await
        {
            Ok((resp, provider_key)) => {
                // 记录该模型上次成功的供应商，下次路由优先使用
                st.last_provider
                    .lock()
                    .unwrap()
                    .insert(model.clone(), provider.id);
                // 别名重定向：记录上次成功的实际模型，下次该别名优先用它
                if st.model_aliases.contains_key(model.as_str()) {
                    st.last_model
                        .lock()
                        .unwrap()
                        .insert(model.clone(), actual_model.clone());
                }
                tracing::debug!(
                    provider = %provider.name,
                    status = %resp.status(),
                    "proxy: upstream responded"
                );
                return build_response(
                    resp,
                    p_proto,
                    c_proto,
                    need_convert,
                    stream,
                    UsageCtx {
                        consumer_key: consumer_key.clone(),
                        // 用量统计按真实模型记录（别名重定向后的实际后端模型）
                        model: actual_model.clone(),
                        provider_id: provider.id,
                        provider_key,
                        db: st.db.clone(),
                    },
                )
                .await;
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
                if (400..500).contains(&status) && status != 429 {
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
    ctx: UsageCtx,
) -> Result<Response, AppError> {
    let status = resp.status();
    let upstream_ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    tracing::debug!(
        p_proto = ?p_proto,
        c_proto = ?c_proto,
        need_convert,
        stream,
        status = %status,
        upstream_content_type = %upstream_ct,
        "build_response"
    );

    // 流式请求降级判定：
    // 客户端请求 stream=true 时，必须确认上游真的返回了 SSE 流：
    //   - status 必须是 2xx
    //   - content-type 必须包含 text/event-stream
    // 上游有时会用 "HTTP 200 + JSON 错误体" 报错（如路由 404、模型不存在、网关故障等），
    // 此时若仍按 SSE 包装透传，客户端会收到 "empty or malformed response (HTTP 200)"，
    // 必须降级为非流式错误透传，让客户端能看到真实错误。
    let upstream_is_sse = status.is_success() && upstream_ct.contains("text/event-stream");

    if stream && upstream_is_sse {
        if need_convert {
            return stream::stream_convert(resp, p_proto, c_proto, ctx).await;
        }
        return Ok(stream::stream_passthrough(resp, ctx));
    }

    // 非流式路径（或流式请求遇到上游非 SSE 的降级处理）：读完 body 再决定如何响应
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    let val: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            // 上游返回非 JSON 内容（HTML 错误页、纯文本等）
            tracing::warn!(
                status = %status,
                upstream_content_type = %upstream_ct,
                body_preview = %truncate_str(&text, 500),
                "upstream returned non-JSON body"
            );
            return Ok((
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": {
                        "message": format!(
                            "upstream returned non-JSON (status {status}, content-type '{upstream_ct}'): {e}"
                        ),
                        "type": "bad_gateway",
                        "upstream_status": status.as_u16(),
                    }
                })),
            )
                .into_response());
        }
    };

    // 上游是否在报错：
    //   - status 非 2xx → 明确错误
    //   - 客户端请求 stream 但上游 content-type 不是 SSE → 伪成功（如 404 路由错误）
    //   - 其它情况（客户端非流式 + 2xx）→ 正常成功响应
    let is_upstream_error = !status.is_success() || (stream && !upstream_is_sse);
    if is_upstream_error {
        tracing::warn!(
            status = %status,
            upstream_content_type = %upstream_ct,
            client_stream = stream,
            body = %val,
            "upstream returned error body"
        );
        // 透传上游错误体给客户端便于排查；status 修正：
        //   - 上游伪 2xx（非 SSE）→ 502 BAD_GATEWAY（反映网关层面判定失败）
        //   - 上游 4xx/5xx → 保留原状态
        let final_status = if status.is_success() {
            StatusCode::BAD_GATEWAY
        } else {
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY)
        };
        return Ok((final_status, Json(val)).into_response());
    }

    // 正常非流式成功响应：按需协议转换 + 记录 token 用量
    let out = if need_convert {
        converter::resp_convert(&val, p_proto, c_proto)?
    } else {
        val
    };
    if let Some((input, output, total)) = stream::extract_usage(&out) {
        ctx.record(input, output, total);
    }
    Ok((status, Json(out)).into_response())
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

/// 截断字符串到 max 字节，回退到最近的 UTF-8 字符边界，避免切到多字节字符中间导致 panic
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}... (truncated {})", &s[..end], s.len())
}

fn truncate_json(v: &Value, max: usize) -> String {
    truncate_str(&serde_json::to_string(v).unwrap_or_default(), max)
}

/// 鉴权，返回 consumer 提交的 key（用于用量统计）
fn auth(st: &AppState, headers: &HeaderMap) -> Result<String, AppError> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let xkey = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    let presented = bearer.or(xkey).unwrap_or("");
    if st.consumer.check_key(presented) {
        Ok(presented.to_string())
    } else {
        Err(AppError::Unauthorized)
    }
}
