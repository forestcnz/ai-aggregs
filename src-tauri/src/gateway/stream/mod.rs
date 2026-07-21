//! 流式协议转换的对外入口与内部模块组织。
//!
//! 模块布局：
//! - `usage` — `UsageCtx` 用量记录、`extract_usage` 嗅探、`sniff_usage` SSE 解析
//! - `config` — `StreamConfig` 心跳/超时配置、`append_utf8_safe` UTF-8 边界处理
//! - `pipeline` — `StreamConverter` trait、`Noop` 直通
//! - `crate::gateway::ir::stream_codec` — 真正的转换器实现：`IrStreamConverter`
//!   （基于 `ChunkEvent` IR，4 个 parser + 3 个 emitter，覆盖所有 6 个方向）
//!
//! 本模块（`mod.rs`）仅暴露 `stream_passthrough*` / `stream_convert*` 公共入口，
//! 处理 SSE 收发循环、心跳与首字超时、UTF-8 边界、用量统计。

mod config;
mod pipeline;
mod usage;

use axum::body::Body;
use axum::response::Response;
use bytes::{Bytes, BytesMut};

use crate::config::types::Protocol;
use crate::gateway::ir::stream_codec::IrStreamConverter;
use crate::infra::error::AppError;

// 公开 API
pub use config::StreamConfig;
pub use pipeline::StreamConverter;
pub use usage::{extract_usage, UsageCtx};

// 仅 mod.rs 内使用的 helper
use config::{append_utf8_safe, KEEPALIVE_LINE};
use pipeline::Noop;
use usage::sniff_usage;

// ===================== 公共入口 =====================

/// 便捷入口（使用全局默认 StreamConfig）。handler.rs 已迁移到 _with_config 版本。
#[allow(dead_code)]
pub fn stream_passthrough(resp: reqwest::Response, ctx: UsageCtx) -> Response {
    stream_passthrough_with_config(resp, ctx, StreamConfig::default())
}

pub fn stream_passthrough_with_config(
    resp: reqwest::Response,
    ctx: UsageCtx,
    config: StreamConfig,
) -> Response {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = BytesMut::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut last_usage: Option<(u64, u64, u64)> = None;
        let mut first_chunk_received = false;

        // 心跳 interval
        let mut keepalive_ticker = config
            .keepalive_interval
            .map(tokio::time::interval);
        if let Some(ref mut ticker) = keepalive_ticker {
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // 第一 tick 立即触发，跳过（不想在首字前就发心跳）
            let _ = ticker.tick().await;
        }
        let first_output_deadline = config
            .first_output_timeout
            .map(|d| tokio::time::Instant::now() + d);

        loop {
            tokio::select! {
                // 上游 chunk 到达
                chunk = stream.next() => {
                    match chunk {
                        Some(Ok(c)) => {
                            first_chunk_received = true;
                            // 嗅探 SSE data 行中的 usage 字段
                            append_utf8_safe(&mut buf, &mut utf8_remainder, &c);
                            while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                                let line_bytes = buf.split_to(nl + 1);
                                let s = String::from_utf8_lossy(&line_bytes);
                                let s = s.trim();
                                if let Some(data) = s.strip_prefix("data:").map(str::trim) {
                                    sniff_usage(data, &mut last_usage);
                                }
                            }
                            // 转发原始字节给客户端
                            if tx.send(Ok(c)).await.is_err() {
                                return;
                            }
                        }
                        Some(Err(e)) => {
                            tracing::error!(err = ?e, "upstream stream read error (passthrough)");
                            return;
                        }
                        None => break, // 上游正常关闭
                    }
                }
                // 心跳定时器
                _ = async {
                    if let Some(ref mut ticker) = keepalive_ticker {
                        ticker.tick().await;
                    } else {
                        // 未配置心跳：永远不触发
                        std::future::pending::<()>().await;
                    }
                } => {
                    if !maybe_send_keepalive(&tx).await {
                        return;
                    }
                }
                // 首字超时（仅在上游尚未产出第一个 chunk 时生效）
                _ = async {
                    if let Some(deadline) = first_output_deadline {
                        if first_chunk_received {
                            std::future::pending::<()>().await;
                        } else {
                            tokio::time::sleep_until(deadline).await;
                        }
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    tracing::warn!("stream first-output timeout (passthrough), aborting");
                    let _ = tx.send(Ok(Bytes::from(
                        "event: error\ndata: {\"type\":\"first_output_timeout\"}\n\n"
                    ))).await;
                    break;
                }
            }
        }

        if let Some((i, o, t)) = last_usage {
            ctx.record(i, o, t);
        }
    });

    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    sse_response(body)
}

/// 便捷入口（使用全局默认 StreamConfig）。handler.rs 已迁移到 _with_config 版本。
#[allow(dead_code)]
pub async fn stream_convert(
    resp: reqwest::Response,
    src: Protocol,
    dst: Protocol,
    ctx: UsageCtx,
) -> Result<Response, AppError> {
    stream_convert_with_config(resp, src, dst, ctx, StreamConfig::default()).await
}

pub async fn stream_convert_with_config(
    resp: reqwest::Response,
    src: Protocol,
    dst: Protocol,
    ctx: UsageCtx,
    config: StreamConfig,
) -> Result<Response, AppError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let mut conv = make_converter(src, dst);

    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut buf = BytesMut::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut cur_event: Option<String> = None;
        let mut cur_data = String::new();
        let mut stream = resp.bytes_stream();
        let mut last_usage: Option<(u64, u64, u64)> = None;
        let mut first_chunk_received = false;

        // 心跳 interval
        let mut keepalive_ticker = config
            .keepalive_interval
            .map(tokio::time::interval);
        if let Some(ref mut ticker) = keepalive_ticker {
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let _ = ticker.tick().await; // 跳过首 tick
        }
        let first_output_deadline = config
            .first_output_timeout
            .map(|d| tokio::time::Instant::now() + d);

        loop {
            tokio::select! {
                // 上游 chunk 到达
                chunk = stream.next() => {
                    let chunk = match chunk {
                        Some(Ok(c)) => c,
                        Some(Err(e)) => {
                            tracing::error!(err = ?e, "upstream stream read error (decoding response body)");
                            break;
                        }
                        None => break, // 上游正常关闭
                    };
                    first_chunk_received = true;
                    append_utf8_safe(&mut buf, &mut utf8_remainder, &chunk);

                        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
                            let line_bytes = buf.split_to(nl + 1);
                            let mut s = String::from_utf8_lossy(&line_bytes).into_owned();
                            if s.ends_with('\n') { s.pop(); }
                            if s.ends_with('\r') { s.pop(); }

                            if s.is_empty() {
                                if !cur_data.is_empty() {
                                    sniff_usage(&cur_data, &mut last_usage);
                                    let payloads = conv.on_event(cur_event.as_deref(), &cur_data);
                                    for p in payloads {
                                        sniff_usage(&p, &mut last_usage);
                                        let line = make_sse_line(&p);
                                        if tx.send(Ok(line.into_bytes().into())).await.is_err() {
                                            return;
                                        }
                                    }
                                }
                                cur_event = None;
                                cur_data.clear();
                            } else if let Some(e) = s.strip_prefix("event:") {
                                cur_event = Some(e.trim().to_string());
                            } else if let Some(d) = s.strip_prefix("data:") {
                                let d = d.strip_prefix(' ').unwrap_or(d);
                                if !cur_data.is_empty() { cur_data.push('\n'); }
                                cur_data.push_str(d);
                            } else if s.starts_with(':') {
                                // SSE 注释行，忽略
                            }
                        }
                }
                // 心跳定时器
                _ = async {
                    if let Some(ref mut ticker) = keepalive_ticker {
                        ticker.tick().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if !maybe_send_keepalive(&tx).await {
                        return;
                    }
                }
                // 首字超时（仅在上游尚未产出第一个 chunk 时生效）
                _ = async {
                    if let Some(deadline) = first_output_deadline {
                        if first_chunk_received {
                            std::future::pending::<()>().await;
                        } else {
                            tokio::time::sleep_until(deadline).await;
                        }
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    tracing::warn!("stream first-output timeout (convert), aborting");
                    let _ = tx.send(Ok(Bytes::from(
                        "event: error\ndata: {\"type\":\"first_output_timeout\"}\n\n"
                    ))).await;
                    break;
                }
            }
        }

        // 收尾：flush utf8_remainder + 处理残留数据 + on_done
        if !utf8_remainder.is_empty() {
            buf.extend_from_slice(&utf8_remainder);
            utf8_remainder.clear();
        }
        // 处理 buf 中剩余的行
        while let Some(nl) = buf.iter().position(|b| *b == b'\n') {
            let line_bytes = buf.split_to(nl + 1);
            let mut s = String::from_utf8_lossy(&line_bytes).into_owned();
            if s.ends_with('\n') { s.pop(); }
            if s.ends_with('\r') { s.pop(); }
            if s.is_empty() {
                if !cur_data.is_empty() {
                    sniff_usage(&cur_data, &mut last_usage);
                    for p in conv.on_event(cur_event.as_deref(), &cur_data) {
                        sniff_usage(&p, &mut last_usage);
                        let line = make_sse_line(&p);
                        let _ = tx.send(Ok(line.into_bytes().into())).await;
                    }
                }
                cur_event = None;
                cur_data.clear();
            } else if let Some(e) = s.strip_prefix("event:") {
                cur_event = Some(e.trim().to_string());
            } else if let Some(d) = s.strip_prefix("data:") {
                let d = d.strip_prefix(' ').unwrap_or(d);
                if !cur_data.is_empty() { cur_data.push('\n'); }
                cur_data.push_str(d);
            }
        }
        // 处理最后一条未闭合的事件
        if !cur_data.is_empty() {
            sniff_usage(&cur_data, &mut last_usage);
            for p in conv.on_event(cur_event.as_deref(), &cur_data) {
                sniff_usage(&p, &mut last_usage);
                let line = make_sse_line(&p);
                let _ = tx.send(Ok(line.into_bytes().into())).await;
            }
        }
        for p in conv.on_done() {
            sniff_usage(&p, &mut last_usage);
            let line = make_sse_line(&p);
            let _ = tx.send(Ok(line.into_bytes().into())).await;
        }

        if let Some((i, o, t)) = last_usage {
            ctx.record(i, o, t);
        }
    });

    let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    Ok(sse_response(body))
}

fn sse_response(body: Body) -> Response {
    Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("connection", "keep-alive")
        .body(body)
        .unwrap()
}

fn make_sse_line(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

/// 向 channel 发送心跳或返回是否应中止
async fn maybe_send_keepalive(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> bool {
    tx.send(Ok(Bytes::from(KEEPALIVE_LINE)))
        .await
        .is_ok()
}

// ===================== make_converter =====================

/// 根据 `(src, dst)` 协议对构造合适的流转换器。
///
/// IR 化后：所有跨协议方向都通过 `IrStreamConverter` 走 src→IR→dst 单跳，
/// 同协议直通走 `Noop`。
pub(crate) fn make_converter(src: Protocol, dst: Protocol) -> Box<dyn StreamConverter> {
    if src == dst {
        return Box::new(Noop);
    }
    Box::new(IrStreamConverter::new(src, dst))
}
