use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use serde_json::{json, Value};

use crate::config::types::Protocol;
use crate::gateway::convert_helpers::extract_cache_field;
use crate::gateway::converter::{
    created_now, map_finish_reason_chat_to_anthropic, map_stop_reason_anthropic_to_chat, rand_id,
};
use crate::infra::db;
use crate::infra::error::AppError;

// ===================== 用量统计 =====================

/// 流式/非流式请求的用量记录上下文
pub struct UsageCtx {
    pub consumer_key: String,
    pub model: String,
    pub provider_id: i64,
    pub provider_key: String,
    pub db: Arc<Mutex<rusqlite::Connection>>,
}

impl UsageCtx {
    /// 记录 token 用量。在 spawn_blocking 中执行同步 DB 操作，
    /// 避免阻塞 tokio runtime 的 async worker thread（用量表行数大时聚合写入可能耗时）。
    pub fn record(&self, input: u64, output: u64, total: u64) {
        if input == 0 && output == 0 {
            return;
        }
        let consumer_key = self.consumer_key.clone();
        let model = self.model.clone();
        let provider_id = self.provider_id;
        let provider_key = self.provider_key.clone();
        let db = self.db.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Ok(conn) = db.lock() {
                let _ = db::record_usage(&conn, &consumer_key, &model, input, output, total);
                let _ = db::record_provider_usage(
                    &conn,
                    provider_id,
                    &provider_key,
                    &model,
                    input,
                    output,
                    total,
                );
            }
        });
    }
}

/// 从任意 JSON 值中提取 token 用量（兼容 Chat / Anthropic / Responses 三种格式）。
/// 返回 (input, output, total)。Anthropic 的 cache_creation/cache_read 也计入 input，
/// 与上游计费规则一致（prompt cache 写入/读取的 token 同样按输入价计费）。
pub fn extract_usage(v: &Value) -> Option<(u64, u64, u64)> {
    // Chat 风格：usage.prompt_tokens / completion_tokens / total_tokens
    if let Some(u) = v.get("usage") {
        if let Some(pt) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
            let ct = u
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            // 兼容 OpenAI 多种 cache token 字段命名：
            // - 顶层（Anthropic 标准 / Kimi 等）：cache_read_input_tokens / cache_creation_input_tokens
            // - nested（OpenAI 标准 / 别名）：prompt_tokens_details.{cached,cache_write,cache_creation}_tokens
            //   + input_tokens_details.cache_write_tokens
            // 顶层优先于 nested（兼容性最高）
            let cached = extract_cache_field(
                u,
                &["cache_read_input_tokens"],
                &[("prompt_tokens_details", "cached_tokens")],
            );
            let cache_creation = extract_cache_field(
                u,
                &["cache_creation_input_tokens"],
                &[
                    ("prompt_tokens_details", "cache_write_tokens"),
                    ("prompt_tokens_details", "cache_creation_tokens"),
                    ("input_tokens_details", "cache_write_tokens"),
                ],
            );
            // 加法哲学：cache 部分（命中+写入）同样按输入价计费，与上游计费规则一致
            let prompt_total = pt + cached + cache_creation;
            let tt = u
                .get("total_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(prompt_total + ct);
            return Some((prompt_total, ct, tt));
        }
        // Anthropic / Responses 风格：usage.input_tokens / output_tokens
        // 加上 cache_creation_input_tokens / cache_read_input_tokens
        if let Some(it) = u.get("input_tokens").and_then(|x| x.as_u64()) {
            let cache_creation = u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let cache_read = u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let total_input = it + cache_creation + cache_read;
            return Some((total_input, ot, total_input + ot));
        }
    }
    // Responses completed 事件：response.usage.input_tokens
    if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
        if let Some(it) = u.get("input_tokens").and_then(|x| x.as_u64()) {
            let cache_creation = u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let cache_read = u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let total_input = it + cache_creation + cache_read;
            return Some((total_input, ot, total_input + ot));
        }
    }
    None
}

/// 从单个 SSE payload 字符串中嗅探用量，更新 last_usage
fn sniff_usage(payload: &str, last_usage: &mut Option<(u64, u64, u64)>) {
    if payload == "[DONE]" {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        if let Some(u) = extract_usage(&v) {
            *last_usage = Some(u);
        }
    }
}

// ===================== 流式管线配置 =====================

/// 流式请求的超时与心跳配置。
///
/// 所有字段为 `Option<Duration>`：`None` 表示禁用对应特性。
/// 默认值通过 `StreamConfig::default()` 提供（keepalive=15s / first-output=120s / interval=60s）。
///
/// 配置来源（优先级递减）：
/// 1. ProviderConfig 的 `stream_*` 字段（provider 级覆盖）
/// 2. 本模块的全局默认常量
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 心跳间隔：周期性向下游发 SSE 注释 `: keepalive\n\n`，防反代空闲断开
    pub keepalive_interval: Option<Duration>,
    /// 首字超时：上游首个有效 chunk 到达前的最长等待
    pub first_output_timeout: Option<Duration>,
    /// 间隔超时：两个上游 chunk 间的最长间隔
    #[allow(dead_code)]
    pub interval_timeout: Option<Duration>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            keepalive_interval: Some(Duration::from_secs(15)),
            first_output_timeout: Some(Duration::from_secs(120)),
            interval_timeout: Some(Duration::from_secs(60)),
        }
    }
}

impl StreamConfig {
    /// 从 ProviderConfig 的可选字段构造（provider 级覆盖），缺省回退全局默认
    #[allow(dead_code)]
    pub fn from_provider(cfg: &crate::config::types::ProviderConfig) -> Self {
        Self {
            keepalive_interval: cfg
                .stream_keepalive_interval_secs
                .filter(|&v| v > 0)
                .map(Duration::from_secs),
            first_output_timeout: cfg
                .stream_first_output_timeout_secs
                .filter(|&v| v > 0)
                .map(Duration::from_secs),
            interval_timeout: cfg
                .stream_interval_timeout_secs
                .filter(|&v| v > 0)
                .map(Duration::from_secs),
        }
    }
}

/// SSE 心跳行（注释格式，不影响 SSE 事件解析）
const KEEPALIVE_LINE: &str = ": keepalive\n\n";

/// 安全地将 chunk 追加到 buf，处理跨 chunk 的 UTF-8 多字节字符边界。
///
/// 如果 chunk 的尾部是不完整的 UTF-8 序列（多字节字符被 TCP chunk 切断），
/// 将不完整部分暂存到 `remainder`，下次调用时拼接。
/// 避免 `from_utf8_lossy` 产生 U+FFFD 替换字符。
///
/// 借鉴 cc-switch `proxy/sse.rs::append_utf8_safe` 的设计。
fn append_utf8_safe(buf: &mut BytesMut, remainder: &mut Vec<u8>, chunk: &[u8]) {
    // 拼接上次的不完整尾部
    let mut combined: Vec<u8> = std::mem::take(remainder);
    combined.extend_from_slice(chunk);

    // 尝试将整块作为 UTF-8 解析
    match std::str::from_utf8(&combined) {
        Ok(_) => {
            // 全部合法
            buf.extend_from_slice(&combined);
        }
        Err(e) => {
            let safe_len = e.valid_up_to();
            buf.extend_from_slice(&combined[..safe_len]);
            // 剩余部分存入 remainder
            // e.error_len() == None 表示不完整序列（尾部被切断），下次拼接即可
            // e.error_len() == Some(_) 表示真正的非法字节，也会被暂存（下次仍会失败，但不丢数据）
            remainder.extend_from_slice(&combined[safe_len..]);
        }
    }
}

/// 向 channel 发送心跳或返回是否应中止
async fn maybe_send_keepalive(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> bool {
    tx.send(Ok(Bytes::from(KEEPALIVE_LINE)))
        .await
        .is_ok()
}

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

// ===================== StreamConverter trait =====================

pub trait StreamConverter: Send {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String>;
    fn on_done(&mut self) -> Vec<String>;
}

impl StreamConverter for Box<dyn StreamConverter> {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        (**self).on_event(event, data)
    }
    fn on_done(&mut self) -> Vec<String> {
        (**self).on_done()
    }
}

struct Chained<A: StreamConverter, B: StreamConverter>(A, B);

impl<A: StreamConverter, B: StreamConverter> StreamConverter for Chained<A, B> {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        let mid = self.0.on_event(event, data);
        let mut out = Vec::new();
        for p in mid {
            out.extend(self.1.on_event(None, &p));
        }
        out
    }
    fn on_done(&mut self) -> Vec<String> {
        let mid = self.0.on_done();
        let mut out = Vec::new();
        for p in mid {
            out.extend(self.1.on_event(None, &p));
        }
        out.extend(self.1.on_done());
        out
    }
}

struct Noop;
impl StreamConverter for Noop {
    fn on_event(&mut self, _e: Option<&str>, data: &str) -> Vec<String> {
        vec![data.to_string()]
    }
    fn on_done(&mut self) -> Vec<String> {
        vec![]
    }
}

pub(crate) fn make_converter(src: Protocol, dst: Protocol) -> Box<dyn StreamConverter> {
    match (src, dst) {
        (Protocol::Anthropic, Protocol::Chat) => Box::new(AnthropicToChatStream::new()),
        (Protocol::Chat, Protocol::Anthropic) => Box::new(ChatToAnthropicStream::new()),
        (Protocol::Responses, Protocol::Chat) => Box::new(ResponsesToChatStream::new()),
        (Protocol::Chat, Protocol::Responses) => Box::new(ChatToResponsesStream::new()),
        (Protocol::Anthropic, Protocol::Responses) => Box::new(Chained(
            AnthropicToChatStream::new(),
            ChatToResponsesStream::new(),
        )),
        (Protocol::Responses, Protocol::Anthropic) => Box::new(Chained(
            ResponsesToChatStream::new(),
            ChatToAnthropicStream::new(),
        )),
        _ => Box::new(Noop),
    }
}

// ===================== Anthropic -> Chat =====================

struct AnthropicToChatStream {
    sent_role: bool,
    sent_done: bool,
    cur_tc_index: Option<usize>,
    next_tc_index: usize,
    input_tokens: Option<u64>,
    in_thinking: bool,
    chat_id: Option<String>,
    model: Option<String>,
    /// 累积 thinking 块的 signature_delta，message_delta 时一次性回传
    signatures: Vec<String>,
}

impl AnthropicToChatStream {
    fn new() -> Self {
        Self {
            sent_role: false,
            sent_done: false,
            cur_tc_index: None,
            next_tc_index: 0,
            input_tokens: None,
            in_thinking: false,
            chat_id: None,
            model: None,
            signatures: Vec::new(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        let id = self
            .chat_id
            .clone()
            .unwrap_or_else(|| format!("chatcmpl-{}", rand_id()));
        let model = self.model.clone().unwrap_or_default();
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created_now(),
            "model": model,
            "choices":[{
                "index": 0,
                "delta": delta,
                "logprobs": null,
                "finish_reason": finish
            }]
        })
    }
}

impl StreamConverter for AnthropicToChatStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match t {
            "message_start" => {
                self.sent_role = true;
                if let Some(m) = v.get("message") {
                    if let Some(id) = m.get("id").and_then(|x| x.as_str()) {
                        self.chat_id = Some(id.to_string());
                    }
                    if let Some(model) = m.get("model").and_then(|x| x.as_str()) {
                        self.model = Some(model.to_string());
                    }
                    if let Some(u) = m.get("usage") {
                        // input + cache_creation + cache_read 合计为 prompt_tokens
                        let it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        let cc = u
                            .get("cache_creation_input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let cr = u
                            .get("cache_read_input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        self.input_tokens = Some(it + cc + cr);
                    }
                }
                vec![self
                    .chunk(json!({"role":"assistant","content":""}), None)
                    .to_string()]
            }
            "content_block_start" => {
                let cb = v.get("content_block").cloned().unwrap_or(json!({}));
                let cb_type = cb.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match cb_type {
                    "tool_use" => {
                        let id = cb.get("id").cloned().unwrap_or(json!(""));
                        let name = cb.get("name").cloned().unwrap_or(json!(""));
                        let idx = self.next_tc_index;
                        self.next_tc_index += 1;
                        self.cur_tc_index = Some(idx);
                        self.in_thinking = false;
                        vec![self.chunk(json!({
                            "tool_calls":[{"index":idx,"id":id,"type":"function","function":{"name":name,"arguments":""}}]
                        }), None).to_string()]
                    }
                    "thinking" | "redacted_thinking" => {
                        self.in_thinking = true;
                        // redacted_thinking 没有 thinking_delta，直接补一条占位 reasoning_content
                        if cb_type == "redacted_thinking" {
                            vec![self
                                .chunk(json!({"reasoning_content":"[redacted_thinking]"}), None)
                                .to_string()]
                        } else {
                            vec![]
                        }
                    }
                    "server_tool_use" => {
                        // 服务端工具调用（web_search/code_execution 等），转文本说明
                        self.in_thinking = false;
                        let name = cb
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("server_tool");
                        vec![self
                            .chunk(
                                json!({"content":format!("[server_tool_use: {name}]")}),
                                None,
                            )
                            .to_string()]
                    }
                    "web_search_tool_result"
                    | "web_fetch_tool_result"
                    | "code_execution_tool_result"
                    | "bash_code_execution_tool_result"
                    | "text_editor_code_execution_tool_result" => {
                        // 服务端工具结果块（流式中内容不可读），跳过
                        self.in_thinking = false;
                        vec![]
                    }
                    "fallback" => {
                        // 服务端 fallback 块（无 deltas），按 text 处理
                        self.in_thinking = false;
                        if let Some(t) = cb.get("text").and_then(|x| x.as_str()) {
                            if !t.is_empty() {
                                vec![self.chunk(json!({"content":t}), None).to_string()]
                            } else {
                                vec![]
                            }
                        } else {
                            vec![]
                        }
                    }
                    _ => {
                        self.in_thinking = false;
                        vec![]
                    }
                }
            }
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(json!({}));
                let dtype = delta.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match dtype {
                    "text_delta" => {
                        let text = delta.get("text").cloned().unwrap_or(json!(""));
                        vec![self.chunk(json!({"content":text}), None).to_string()]
                    }
                    "thinking_delta" => {
                        let text = delta.get("thinking").cloned().unwrap_or(json!(""));
                        vec![self
                            .chunk(json!({"reasoning_content":text}), None)
                            .to_string()]
                    }
                    // thinking 块的签名增量：累积，等 message_delta 时回传
                    "signature_delta" => {
                        if let Some(s) = delta.get("signature").and_then(|x| x.as_str()) {
                            if !s.is_empty() {
                                self.signatures.push(s.to_string());
                            }
                        }
                        vec![]
                    }
                    "input_json_delta" => {
                        let pj = delta.get("partial_json").cloned().unwrap_or(json!(""));
                        if let Some(idx) = self.cur_tc_index {
                            vec![self
                                .chunk(
                                    json!({
                                        "tool_calls":[{"index":idx,"function":{"arguments":pj}}]
                                    }),
                                    None,
                                )
                                .to_string()]
                        } else {
                            vec![]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                self.cur_tc_index = None;
                self.in_thinking = false;
                vec![]
            }
            "message_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(json!({}));
                let stop_reason = delta
                    .get("stop_reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("end_turn");
                let finish = map_stop_reason_anthropic_to_chat(stop_reason);
                // 把累积的 thinking signature 一次性放在这一帧的 delta 中（多轮 thinking 完整性）
                let mut delta_obj = json!({"content":"","reasoning_content":null});
                if !self.signatures.is_empty() {
                    delta_obj["reasoning_signature"] =
                        json!(std::mem::take(&mut self.signatures).join("\n"));
                }
                let mut frame = self.chunk(delta_obj, Some(&finish));
                if let Some(u) = v.get("usage") {
                    let mut usage = json!({});
                    let input_tok = u
                        .get("input_tokens")
                        .and_then(|x| x.as_u64())
                        .or(self.input_tokens);
                    if let Some(it) = input_tok {
                        usage["prompt_tokens"] = json!(it);
                    }
                    if let Some(ot) = u.get("output_tokens") {
                        usage["completion_tokens"] = ot.clone();
                    }
                    if let (Some(a), Some(b)) =
                        (input_tok, u.get("output_tokens").and_then(|x| x.as_u64()))
                    {
                        usage["total_tokens"] = json!(a + b);
                    }
                    frame["usage"] = usage;
                } else if let Some(it) = self.input_tokens {
                    frame["usage"] = json!({"prompt_tokens": it});
                }
                vec![frame.to_string()]
            }
            "message_stop" => {
                self.sent_done = true;
                vec!["[DONE]".to_string()]
            }
            _ => vec![],
        }
    }

    fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            vec![]
        } else {
            self.sent_done = true;
            vec!["[DONE]".to_string()]
        }
    }
}

// ===================== Chat -> Anthropic =====================

/// 单个 tool_call 的累积状态，按上游 chat tool_call index 索引。
///
/// 解决三类边界 case：
/// 1. **乱序到达**（DeepSeek/GLM/Zhipu）：上游先发 id+arguments，name 后到。
///    未到位前缓存在 `pending_args`，name 到达后再发 content_block_start。
/// 2. **多 tool_call 并发**：上游可在不同 chunk 发多个 index 的 delta。
/// 3. **late starts 兜底**：name 永远没到时，finish_reason 触发 fallback 宣告。
/// 4. **Copilot 无限空白 bug**：GitHub Copilot 有时在 tool_call arguments 中
///    产生无限连续的空白字符（换行/空格），导致客户端卡死。
///    `consecutive_whitespace` 跟踪连续空白字符数，超过阈值时中止该 tool 流。
const INFINITE_WHITESPACE_THRESHOLD: usize = 500;

#[derive(Default)]
struct ToolBlockState {
    /// Anthropic 端 content_block 的 index（宣告后固定）
    anthropic_index: Option<usize>,
    /// 上游 tool_call id（可能跨多 chunk 累积）
    id: String,
    /// 上游 tool_call name（可能跨多 chunk 累积）
    name: String,
    /// name 到达前的 arguments 缓冲，宣告时一次性 flush
    pending_args: String,
    /// 是否已发 content_block_start
    announced: bool,
    /// 连续空白字符计数（Copilot 无限空白 bug 检测）
    consecutive_whitespace: usize,
    /// 是否因无限空白 bug 被中止
    aborted: bool,
}

struct ChatToAnthropicStream {
    started: bool,
    sent_done: bool,
    next_block: usize,
    cur_block: Option<(usize, String)>,
    /// 上游（chat）在 finish_reason 帧发来的 reasoning_signature；
    /// 关闭 thinking 块前用它发 signature_delta，保证多轮 thinking 完整性
    pending_signature: Option<String>,
    /// 按 chat tool_call index 索引的累积状态。支持 DeepSeek 等乱序上游。
    tool_blocks: HashMap<usize, ToolBlockState>,
    /// 当前 tool_use 块是否有 input_json_delta（用于空 args 补 "{}"）
    cur_tool_had_delta: bool,
}

impl ChatToAnthropicStream {
    fn new() -> Self {
        Self {
            started: false,
            sent_done: false,
            next_block: 0,
            cur_block: None,
            pending_signature: None,
            tool_blocks: HashMap::new(),
            cur_tool_had_delta: false,
        }
    }

    /// 关闭当前 block；若是 thinking 块且有累积的 signature，
    /// 在 content_block_stop 之前先发 signature_delta 事件。
    ///
    /// 若是 tool_use 块且无任何 input_json_delta，主动补一个 "{}"——
    /// Claude SDK 等严格客户端只从 delta 累积 input，null input 会导致后续工具执行失败。
    fn close_cur_block(&mut self, out: &mut Vec<String>) {
        if let Some((idx, ty)) = self.cur_block.take() {
            if ty == "thinking" {
                if let Some(sig) = self.pending_signature.take() {
                    out.push(signature_delta_event(idx, &sig));
                }
            }
            if ty == "tool_use" && !self.cur_tool_had_delta {
                // 空 arguments 补 "{}"，避免客户端收到 null input
                out.push(input_json_delta_event(idx, "{}"));
            }
            self.cur_tool_had_delta = false;
            out.push(content_block_stop_frame(idx));
        }
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((idx, ref ty)) = self.cur_block {
            if ty == "text" {
                return idx;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, "text".into()));
        out.push(content_block_start_text_frame(idx));
        idx
    }

    fn ensure_thinking(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((idx, ref ty)) = self.cur_block {
            if ty == "thinking" {
                return idx;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, "thinking".into()));
        out.push(content_block_start_thinking_frame(idx));
        idx
    }

    /// finish_reason 触发时的 late starts 兜底：
    /// 上游先发 arguments 但 name 永远没到（极端边界 case），
    /// 用 fallback 名宣告，避免参数数据丢失。
    fn finalize_pending_tool_blocks(&mut self, out: &mut Vec<String>) {
        let mut late_starts: Vec<(usize, String, String, String)> = Vec::new();
        for (chat_idx, state) in self.tool_blocks.iter_mut() {
            if state.announced || state.aborted {
                continue;
            }
            // 完全空的状态跳过（理论上不会出现在 map 中，但防御性判断）
            if state.pending_args.is_empty() && state.id.is_empty() && state.name.is_empty() {
                continue;
            }
            let bidx = self.next_block;
            self.next_block += 1;
            state.anthropic_index = Some(bidx);
            state.announced = true;
            let fallback_id = if state.id.is_empty() {
                format!("tool_call_{chat_idx}")
            } else {
                state.id.clone()
            };
            let fallback_name = if state.name.is_empty() {
                "unknown_tool".to_string()
            } else {
                state.name.clone()
            };
            let pending = std::mem::take(&mut state.pending_args);
            late_starts.push((bidx, fallback_id, fallback_name, pending));
        }
        // 按 anthropic_index 排序保证输出顺序稳定
        late_starts.sort_unstable_by_key(|(idx, _, _, _)| *idx);
        for (bidx, id, name, pending) in late_starts {
            out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));
            if !pending.is_empty() {
                out.push(input_json_delta_event(bidx, &pending));
            } else {
                // 兜底：无 arguments 也补 "{}"
                out.push(input_json_delta_event(bidx, "{}"));
            }
            out.push(content_block_stop_frame(bidx));
        }
    }
}

impl StreamConverter for ChatToAnthropicStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        if data == "[DONE]" {
            let mut out = vec![];
            self.close_cur_block(&mut out);
            // 上游 [DONE] 前若仍有未宣告的 tool_block，兜底宣告
            self.finalize_pending_tool_blocks(&mut out);
            out.push(message_stop_event());
            self.sent_done = true;
            return out;
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out: Vec<String> = vec![];

        if !self.started {
            self.started = true;
            out.push(message_start_event());
        }

        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            return out;
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));

        // 累积上游 chat 的 reasoning_signature（finish_reason 那一帧才出现）
        if let Some(sig) = delta.get("reasoning_signature").and_then(|x| x.as_str()) {
            if !sig.is_empty() {
                self.pending_signature = Some(sig.to_string());
            }
        }

        if let Some(rc) = delta.get("reasoning_content") {
            if let Some(t) = rc.as_str() {
                if !t.is_empty() {
                    let idx = self.ensure_thinking(&mut out);
                    out.push(thinking_delta_event(idx, t));
                }
            }
        }

        if let Some(content) = delta.get("content") {
            if let Some(t) = content.as_str() {
                if !t.is_empty() {
                    let idx = self.ensure_text(&mut out);
                    out.push(text_delta_event(idx, t));
                }
            }
        }

        // tool_calls 处理：支持乱序到达（id/name/arguments 可任意顺序）
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let chat_idx = tc
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as usize)
                    .unwrap_or(0);

                // 取或创建状态（按 chat tool_call index 索引，支持多并发）
                // 在作用域内完成累积 + 决策，避免 self 借用冲突
                let (announce_action, emit_action) = {
                    let state = self
                        .tool_blocks
                        .entry(chat_idx)
                        .or_default();

                    // Copilot 无限空白 bug：已中止的 tool_call 跳过所有后续处理
                    if state.aborted {
                        (None, None)
                    } else {
                        // 累积 id（可能跨多 chunk 到达）
                        if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                            state.id = id.to_string();
                        }
                        // 累积 name（DeepSeek/GLM 可能比 id/arguments 后到）
                        if let Some(name) = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                        {
                            state.name = name.to_string();
                        }
                        // 累积 arguments（无论是否宣告，先入 pending_args）
                        if let Some(args) = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                        {
                            if !args.is_empty() {
                                // Copilot 无限空白 bug 检测：跟踪连续空白字符
                                for ch in args.chars() {
                                    if ch.is_whitespace() {
                                        state.consecutive_whitespace += 1;
                                    } else {
                                        state.consecutive_whitespace = 0;
                                    }
                                }
                                if state.consecutive_whitespace >= INFINITE_WHITESPACE_THRESHOLD {
                                    tracing::warn!(
                                        chat_idx,
                                        name = %state.name,
                                        consecutive_ws = state.consecutive_whitespace,
                                        "Copilot 无限空白 bug 检测：中止 tool_call 流"
                                    );
                                    state.aborted = true;
                                    state.pending_args.clear();
                                } else {
                                    state.pending_args.push_str(args);
                                }
                            }
                        }

                        // 决策（aborted 可能刚被上面的空白检测置位）
                        if state.aborted {
                            (None, None)
                        } else if !state.announced
                            && !state.id.is_empty()
                            && !state.name.is_empty()
                        {
                            let pending = std::mem::take(&mut state.pending_args);
                            (
                                Some((state.id.clone(), state.name.clone(), pending)),
                                None,
                            )
                        } else if state.announced && !state.pending_args.is_empty() {
                            let args = std::mem::take(&mut state.pending_args);
                            (None, Some((state.anthropic_index, args)))
                        } else {
                            (None, None)
                        }
                    }
                };

                // 执行宣告（已释放 state 借用，可自由调用 self 方法）
                if let Some((id, name, pending)) = announce_action {
                    self.close_cur_block(&mut out);
                    let bidx = self.next_block;
                    self.next_block += 1;
                    if let Some(state) = self.tool_blocks.get_mut(&chat_idx) {
                        state.anthropic_index = Some(bidx);
                        state.announced = true;
                    }
                    self.cur_block = Some((bidx, "tool_use".into()));
                    self.cur_tool_had_delta = false;

                    out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));

                    // 宣告时 flush pending_args（DeepSeek 场景：arguments 在 name 之前已到）
                    if !pending.is_empty() {
                        out.push(input_json_delta_event(bidx, &pending));
                        self.cur_tool_had_delta = true;
                    }
                } else if let Some((Some(bidx), args)) = emit_action {
                    // 已宣告的 tool_block 直接发 input_json_delta
                    out.push(input_json_delta_event(bidx, &args));
                    self.cur_tool_had_delta = true;
                }
            }
        }

        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            self.close_cur_block(&mut out);
            // 兜底：name 永远没到的工具（极端边界 case）
            self.finalize_pending_tool_blocks(&mut out);
            let stop_reason = map_finish_reason_chat_to_anthropic(fr);
            let mut usage = json!({});
            if let Some(u) = v.get("usage") {
                if let Some(pt) = u.get("prompt_tokens") {
                    usage["input_tokens"] = pt.clone();
                }
                if let Some(ct) = u.get("completion_tokens") {
                    usage["output_tokens"] = ct.clone();
                }
            }
            out.push(message_delta_event(stop_reason, usage));
        }

        out
    }

    fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            return vec![];
        }
        self.sent_done = true;
        let mut out = vec![];
        self.close_cur_block(&mut out);
        out.push(message_stop_event());
        out
    }
}

// ---- Chat -> Anthropic 事件构造 helper ----

fn message_start_event() -> String {
    json!({
        "type":"message_start",
        "message":{
            "id": format!("msg_{}", rand_id()),
            "type":"message","role":"assistant","model":"",
            "content":[],"stop_reason":null,
            "usage":{"input_tokens":0,"output_tokens":0}
        }
    })
    .to_string()
}
fn content_block_start_text_frame(idx: usize) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"text","text":""}
    })
    .to_string()
}
fn content_block_start_thinking_frame(idx: usize) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"thinking","thinking":"","signature":""}
    })
    .to_string()
}
fn thinking_delta_event(idx: usize, text: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"thinking_delta","thinking":text}
    })
    .to_string()
}
/// thinking 块的签名增量；客户端在多轮 thinking 中必须把它和 thinking 一起回传
fn signature_delta_event(idx: usize, signature: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"signature_delta","signature":signature}
    })
    .to_string()
}
fn content_block_start_tool_event(idx: usize, id: Value, name: Value) -> String {
    json!({
        "type":"content_block_start","index":idx,
        "content_block":{"type":"tool_use","id":id,"name":name,"input":{}}
    })
    .to_string()
}
fn text_delta_event(idx: usize, text: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"text_delta","text":text}
    })
    .to_string()
}
fn input_json_delta_event(idx: usize, partial: &str) -> String {
    json!({
        "type":"content_block_delta","index":idx,
        "delta":{"type":"input_json_delta","partial_json":partial}
    })
    .to_string()
}
fn content_block_stop_frame(idx: usize) -> String {
    json!({"type":"content_block_stop","index":idx}).to_string()
}
fn message_delta_event(stop_reason: String, usage: Value) -> String {
    json!({
        "type":"message_delta",
        "delta":{"stop_reason":stop_reason,"stop_sequence":null},
        "usage":usage
    })
    .to_string()
}
fn message_stop_event() -> String {
    json!({"type":"message_stop"}).to_string()
}

// ===================== Responses -> Chat =====================

struct ResponsesToChatStream {
    sent_role: bool,
    sent_done: bool,
    tool_map: HashMap<u64, usize>,
    next_tc: usize,
}

impl ResponsesToChatStream {
    fn new() -> Self {
        Self {
            sent_role: false,
            sent_done: false,
            tool_map: HashMap::new(),
            next_tc: 0,
        }
    }
}

impl StreamConverter for ResponsesToChatStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match t {
            "response.created" | "response.in_progress" => {
                if !self.sent_role {
                    self.sent_role = true;
                    vec![json!({
                        "choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]
                    })
                    .to_string()]
                } else {
                    vec![]
                }
            }
            // 推理增量（多种事件名兼容）：
            //   response.reasoning.delta / .done（旧格式，delta 是字符串）
            //   response.reasoning_summary.delta / .done（旧格式）
            //   response.reasoning_summary_text.delta / .done（新格式）
            "response.reasoning.delta"
            | "response.reasoning_summary.delta"
            | "response.reasoning_summary_text.delta" => {
                let d = match v.get("delta") {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                if d.is_empty() {
                    vec![]
                } else {
                    vec![json!({
                        "choices":[{"index":0,"delta":{"reasoning_content":d},"finish_reason":null}]
                    })
                    .to_string()]
                }
            }
            "response.reasoning.done"
            | "response.reasoning_summary.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => {
                // 这些是终止/分部事件，chat 协议无对应，忽略
                vec![]
            }
            // 拒绝内容（refusal）
            "response.refusal.delta" => {
                let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                if d.is_empty() {
                    vec![]
                } else {
                    vec![json!({
                        "choices":[{"index":0,"delta":{"content":d},"finish_reason":null}]
                    })
                    .to_string()]
                }
            }
            "response.refusal.done" => vec![],
            // 服务端工具事件（web_search / file_search / image_generation / code_interpreter），
            // Chat 协议无法表达，忽略
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(json!({}));
                let itype = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match itype {
                    "function_call" => {
                        let oi = v.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0);
                        let call_id = item.get("call_id").cloned().unwrap_or(json!(""));
                        let name = item.get("name").cloned().unwrap_or(json!(""));
                        let tc_idx = self.next_tc;
                        self.next_tc += 1;
                        self.tool_map.insert(oi, tc_idx);
                        vec![json!({
                            "choices":[{"index":0,"delta":{
                                "tool_calls":[{"index":tc_idx,"id":call_id,"type":"function","function":{"name":name,"arguments":""}}]
                            },"finish_reason":null}]
                        })
                        .to_string()]
                    }
                    "web_search_call"
                    | "file_search_call"
                    | "image_generation_call"
                    | "code_interpreter_call"
                    | "computer_call"
                    | "mcp_call" => {
                        // 服务端工具调用 item，转文本说明
                        let label = match itype {
                            "web_search_call" => "web_search",
                            "file_search_call" => "file_search",
                            "image_generation_call" => "image_generation",
                            "code_interpreter_call" => "code_interpreter",
                            "computer_call" => "computer",
                            _ => "mcp",
                        };
                        vec![json!({
                            "choices":[{"index":0,"delta":{"content":format!("[{label}]")},"finish_reason":null}]
                        })
                        .to_string()]
                    }
                    _ => vec![],
                }
            }
            "response.output_text.delta" => {
                let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                if d.is_empty() {
                    vec![]
                } else {
                    vec![json!({
                        "choices":[{"index":0,"delta":{"content":d},"finish_reason":null}]
                    })
                    .to_string()]
                }
            }
            // 终止/收尾事件，忽略（content 由 delta 增量累积，客户端自行拼接）
            "response.output_text.done"
            | "response.output_text.annotation.added"
            | "response.output_text.annotation.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_item.done"
            | "response.function_call_arguments.done" => vec![],
            "response.function_call_arguments.delta" => {
                let oi = v.get("output_index").and_then(|x| x.as_u64()).unwrap_or(0);
                let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
                if let Some(&tc_idx) = self.tool_map.get(&oi) {
                    vec![json!({
                        "choices":[{"index":0,"delta":{
                            "tool_calls":[{"index":tc_idx,"function":{"arguments":d}}]
                        },"finish_reason":null}]
                    })
                    .to_string()]
                } else {
                    vec![]
                }
            }
            "response.completed"
            | "response.incomplete"
            | "response.failed"
            | "response.cancelled" => {
                let has_tool = !self.tool_map.is_empty();
                let finish = if t == "response.failed" || t == "response.cancelled" {
                    // 失败/取消没有完美对应，回退 stop 避免客户端报错
                    "stop"
                } else if has_tool {
                    "tool_calls"
                } else if t == "response.incomplete" {
                    "length"
                } else {
                    "stop"
                };
                let mut frame = json!({
                    "choices":[{"index":0,"delta":{},"finish_reason":finish}]
                });
                if let Some(resp) = v.get("response") {
                    if let Some(u) = resp.get("usage") {
                        let it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        let cc = u
                            .get("cache_creation_input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let cr = u
                            .get("cache_read_input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0);
                        let ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                        let total_input = it + cc + cr;
                        let mut usage = json!({
                            "prompt_tokens": total_input,
                            "completion_tokens": ot,
                            "total_tokens": total_input + ot
                        });
                        if cc > 0 {
                            usage["cache_creation_input_tokens"] = json!(cc);
                        }
                        if cr > 0 {
                            usage["cache_read_input_tokens"] = json!(cr);
                        }
                        frame["usage"] = usage;
                    }
                }
                self.sent_done = true;
                vec![frame.to_string(), "[DONE]".to_string()]
            }
            // 服务端工具增量（web_search_call.* / file_search_call.* / 等）忽略
            _ if t.starts_with("response.web_search_call.")
                || t.starts_with("response.file_search_call.")
                || t.starts_with("response.image_generation_call.")
                || t.starts_with("response.code_interpreter_call.")
                || t.starts_with("response.computer_call.") =>
            {
                vec![]
            }
            _ => vec![],
        }
    }

    fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            vec![]
        } else {
            self.sent_done = true;
            vec![
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}).to_string(),
                "[DONE]".to_string(),
            ]
        }
    }
}

// ===================== Chat -> Responses =====================

fn chat_usage_to_responses_usage(u: &Value) -> Value {
    let mut out = json!({});
    if let Some(pt) = u.get("prompt_tokens") {
        out["input_tokens"] = pt.clone();
    }
    if let Some(ct) = u.get("completion_tokens") {
        out["output_tokens"] = ct.clone();
    }
    if let Some(tt) = u.get("total_tokens") {
        out["total_tokens"] = tt.clone();
    }
    if out.get("input_tokens").is_none() {
        out["input_tokens"] = json!(0);
    }
    if out.get("output_tokens").is_none() {
        out["output_tokens"] = json!(0);
    }
    if out.get("total_tokens").is_none() {
        if let (Some(a), Some(b)) = (
            out.get("input_tokens").and_then(|x| x.as_u64()),
            out.get("output_tokens").and_then(|x| x.as_u64()),
        ) {
            out["total_tokens"] = json!(a + b);
        } else {
            out["total_tokens"] = json!(0);
        }
    }
    out
}

/// 单个 function_call item 的累积状态
struct ToolCallState {
    item_id: String,
    call_id: Value,
    name: Value,
    arguments: String,
}

struct ChatToResponsesStream {
    created: bool,
    sent_done: bool,
    next_output_index: usize,
    message_oi: Option<usize>,
    reasoning_oi: Option<usize>,
    /// chat tool index -> output_index
    tool_oi: HashMap<usize, usize>,
    /// output_index -> 累积状态（已开启未关闭的 function_call）
    tool_items: HashMap<usize, ToolCallState>,
    usage: Value,

    // 累积的文本（用于回填 .done / output_item.done / response.completed）
    accumulated_text: String,
    accumulated_reasoning: String,

    // 全局一致的 ID（整个响应生命周期内不变）
    resp_id: String,
    msg_item_id: String,
    reasoning_item_id: String,

    // 当前是否已有未关闭的 message / reasoning item
    message_item_added: bool,
    reasoning_item_added: bool,

    // 最终 output 数组（response.completed 时回填）
    final_output: Vec<Value>,
}

impl ChatToResponsesStream {
    fn new() -> Self {
        Self {
            created: false,
            sent_done: false,
            next_output_index: 0,
            message_oi: None,
            reasoning_oi: None,
            tool_oi: HashMap::new(),
            tool_items: HashMap::new(),
            usage: json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}),
            accumulated_text: String::new(),
            accumulated_reasoning: String::new(),
            resp_id: format!("resp_{}", rand_id()),
            msg_item_id: format!("msg_{}", rand_id()),
            reasoning_item_id: format!("rs_{}", rand_id()),
            message_item_added: false,
            reasoning_item_added: false,
            final_output: Vec::new(),
        }
    }

    fn base_response(&self, status: &str) -> Value {
        json!({
            "id": self.resp_id,
            "object":"response",
            "created_at": created_now(),
            "status":status,
            "output":[]
        })
    }

    /// 首个 chunk 时发送 response.created + response.in_progress
    fn ensure_created(&mut self, out: &mut Vec<String>) {
        if self.created {
            return;
        }
        self.created = true;
        out.push(
            json!({"type":"response.created","response":self.base_response("in_progress")})
                .to_string(),
        );
        out.push(
            json!({"type":"response.in_progress","response":self.base_response("in_progress")})
                .to_string(),
        );
    }

    /// 开启 reasoning item：output_item.added(reasoning) + reasoning_summary_part.added
    fn ensure_reasoning_item(&mut self, out: &mut Vec<String>) {
        if self.reasoning_item_added {
            return;
        }
        self.reasoning_item_added = true;
        let oi = self.next_output_index;
        self.next_output_index += 1;
        self.reasoning_oi = Some(oi);
        out.push(
            json!({
                "type":"response.output_item.added","output_index":oi,
                "item":{
                    "id":self.reasoning_item_id,
                    "type":"reasoning",
                    "summary":[]
                }
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.reasoning_summary_part.added",
                "item_id":self.reasoning_item_id,
                "output_index":oi,
                "summary_index":0,
                "part":{"type":"summary_text","text":""}
            })
            .to_string(),
        );
    }

    /// 关闭 reasoning item：reasoning_summary_text.done + reasoning_summary_part.done + output_item.done
    /// 同时把完整 item 推入 final_output
    fn close_reasoning(&mut self, out: &mut Vec<String>) {
        if !self.reasoning_item_added {
            return;
        }
        self.reasoning_item_added = false;
        let oi = self.reasoning_oi.unwrap_or(0);
        let text = std::mem::take(&mut self.accumulated_reasoning);
        let summary_part = json!({"type":"summary_text","text":text.clone()});
        let item = json!({
            "id":self.reasoning_item_id,
            "type":"reasoning",
            "summary":[summary_part.clone()]
        });
        out.push(
            json!({
                "type":"response.reasoning_summary_text.done",
                "item_id":self.reasoning_item_id,
                "output_index":oi,
                "summary_index":0,
                "text":text
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.reasoning_summary_part.done",
                "item_id":self.reasoning_item_id,
                "output_index":oi,
                "summary_index":0,
                "part":summary_part
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.output_item.done","output_index":oi,
                "item":item.clone()
            })
            .to_string(),
        );
        self.final_output.push(item);
    }

    /// 开启 message item：先关闭未完成的 reasoning，再发 output_item.added(message) + content_part.added
    fn ensure_message_item(&mut self, out: &mut Vec<String>) {
        if self.message_item_added {
            return;
        }
        self.close_reasoning(out);
        self.message_item_added = true;
        let oi = self.next_output_index;
        self.next_output_index += 1;
        self.message_oi = Some(oi);
        out.push(
            json!({
                "type":"response.output_item.added","output_index":oi,
                "item":{
                    "id":self.msg_item_id,
                    "type":"message",
                    "status":"in_progress",
                    "role":"assistant",
                    "content":[]
                }
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.content_part.added",
                "item_id":self.msg_item_id,
                "output_index":oi,
                "content_index":0,
                "part":{"type":"output_text","text":"","annotations":[]}
            })
            .to_string(),
        );
    }

    /// 关闭 message item：output_text.done + content_part.done + output_item.done（回填完整文本）
    fn close_message(&mut self, out: &mut Vec<String>) {
        if !self.message_item_added {
            return;
        }
        self.message_item_added = false;
        let oi = self.message_oi.unwrap_or(0);
        let text = std::mem::take(&mut self.accumulated_text);
        let content_part = json!({"type":"output_text","text":text.clone(),"annotations":[]});
        let item = json!({
            "id":self.msg_item_id,
            "type":"message",
            "status":"completed",
            "role":"assistant",
            "content":[content_part.clone()]
        });
        out.push(
            json!({
                "type":"response.output_text.done",
                "item_id":self.msg_item_id,
                "output_index":oi,
                "content_index":0,
                "text":text
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.content_part.done",
                "item_id":self.msg_item_id,
                "output_index":oi,
                "content_index":0,
                "part":content_part
            })
            .to_string(),
        );
        out.push(
            json!({
                "type":"response.output_item.done","output_index":oi,
                "item":item.clone()
            })
            .to_string(),
        );
        self.final_output.push(item);
    }

    /// 关闭所有未完成的 function_call item
    fn close_tool_calls(&mut self, out: &mut Vec<String>) {
        let mut ois: Vec<usize> = self.tool_items.keys().copied().collect();
        ois.sort_unstable();
        for oi in ois {
            let st = self.tool_items.remove(&oi).unwrap();
            let item = json!({
                "id":st.item_id,
                "type":"function_call",
                "call_id":st.call_id,
                "name":st.name,
                "arguments":st.arguments
            });
            out.push(
                json!({
                    "type":"response.function_call_arguments.done",
                    "output_index":oi,
                    "item_id":item["id"].clone(),
                    "arguments":item["arguments"].clone()
                })
                .to_string(),
            );
            out.push(
                json!({
                    "type":"response.output_item.done","output_index":oi,
                    "item":item.clone()
                })
                .to_string(),
            );
            self.final_output.push(item);
        }
    }

    /// 关闭所有未关闭的 item（reasoning / message / tool_calls）
    fn close_all(&mut self, out: &mut Vec<String>) {
        self.close_message(out);
        self.close_reasoning(out);
        self.close_tool_calls(out);
    }
}

impl StreamConverter for ChatToResponsesStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        if data == "[DONE]" {
            return vec![];
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out: Vec<String> = vec![];
        self.ensure_created(&mut out);

        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            return out;
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));

        // 推理内容：reasoning_summary_text.delta（累积到 accumulated_reasoning）
        if let Some(rc) = delta.get("reasoning_content") {
            if let Some(t) = rc.as_str() {
                if !t.is_empty() {
                    self.ensure_reasoning_item(&mut out);
                    self.accumulated_reasoning.push_str(t);
                    out.push(
                        json!({
                            "type":"response.reasoning_summary_text.delta",
                            "item_id":self.reasoning_item_id,
                            "output_index":self.reasoning_oi.unwrap_or(0),
                            "summary_index":0,
                            "delta":t
                        })
                        .to_string(),
                    );
                }
            }
        }

        // 正文：output_text.delta（累积到 accumulated_text）
        if let Some(content) = delta.get("content").and_then(|x| x.as_str()) {
            if !content.is_empty() {
                self.ensure_message_item(&mut out);
                self.accumulated_text.push_str(content);
                out.push(
                    json!({
                        "type":"response.output_text.delta",
                        "item_id":self.msg_item_id,
                        "output_index":self.message_oi.unwrap_or(0),
                        "content_index":0,
                        "delta":content
                    })
                    .to_string(),
                );
            }
        }

        // 工具调用
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let chat_idx = tc
                    .get("index")
                    .and_then(|x| x.as_u64())
                    .map(|i| i as usize)
                    .unwrap_or(0);
                let has_id = tc.get("id").is_some();
                if has_id {
                    // 新的 function_call：先关闭 message / reasoning
                    self.close_message(&mut out);
                    self.close_reasoning(&mut out);
                    let oi = self.next_output_index;
                    self.next_output_index += 1;
                    self.tool_oi.insert(chat_idx, oi);
                    let call_id = tc.get("id").cloned().unwrap_or(json!(""));
                    let name = tc["function"]["name"].clone();
                    let item_id = format!("fc_{}", rand_id());
                    out.push(
                        json!({
                            "type":"response.output_item.added","output_index":oi,
                            "item":{
                                "id":item_id,
                                "type":"function_call",
                                "call_id":call_id,
                                "name":name,
                                "arguments":""
                            }
                        })
                        .to_string(),
                    );
                    self.tool_items.insert(
                        oi,
                        ToolCallState {
                            item_id,
                            call_id: tc.get("id").cloned().unwrap_or(json!("")),
                            name: tc["function"]["name"].clone(),
                            arguments: String::new(),
                        },
                    );
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        if !args.is_empty() {
                            if let Some(st) = self.tool_items.get_mut(&oi) {
                                st.arguments.push_str(args);
                            }
                            out.push(
                                json!({
                                    "type":"response.function_call_arguments.delta",
                                    "output_index":oi,"delta":args
                                })
                                .to_string(),
                            );
                        }
                    }
                } else if let Some(args) = tc["function"].get("arguments").and_then(|x| x.as_str())
                {
                    if let Some(&oi) = self.tool_oi.get(&chat_idx) {
                        if let Some(st) = self.tool_items.get_mut(&oi) {
                            st.arguments.push_str(args);
                        }
                        out.push(
                            json!({
                                "type":"response.function_call_arguments.delta",
                                "output_index":oi,"delta":args
                            })
                            .to_string(),
                        );
                    }
                }
            }
        }

        // 收尾：关闭所有未关闭的 item，发送 response.completed（output 必须回填完整内容）
        if choice
            .get("finish_reason")
            .and_then(|x| x.as_str())
            .is_some()
        {
            self.close_all(&mut out);
            if let Some(u) = v.get("usage") {
                self.usage = chat_usage_to_responses_usage(u);
            }
            out.push(
                json!({
                    "type":"response.completed",
                    "response":{
                        "id":self.resp_id,
                        "object":"response",
                        "created_at":created_now(),
                        "status":"completed",
                        "output":self.final_output,
                        "usage":self.usage.clone()
                    }
                })
                .to_string(),
            );
            self.sent_done = true;
        }

        out
    }

    fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            return vec![];
        }
        self.sent_done = true;
        let mut out: Vec<String> = vec![];
        self.close_all(&mut out);
        out.push(
            json!({
                "type":"response.completed",
                "response":{
                    "id":self.resp_id,
                    "object":"response",
                    "created_at":created_now(),
                    "status":"completed",
                    "output":self.final_output,
                    "usage":self.usage.clone()
                }
            })
            .to_string(),
        );
        out
    }
}
