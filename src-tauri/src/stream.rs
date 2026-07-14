//! 流式转换：SSE 透传 + 各协议对流式状态机。
//!
//! 透传分支直接转发上游字节流；转换分支逐事件解析上游 SSE，
//! 通过 StreamConverter 状态机产出下游 SSE payload，立即发送，不缓冲完整响应。

use std::collections::HashMap;

use axum::body::Body;
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use serde_json::{json, Value};

use crate::config::Protocol;
use crate::converter::{
    map_finish_reason_chat_to_anthropic, map_stop_reason_anthropic_to_chat,
};
use crate::error::AppError;

// ===================== 公共入口 =====================

/// 透传：原样转发上游 SSE 字节流
pub fn stream_passthrough(resp: reqwest::Response) -> Response {
    let body = Body::from_stream(resp.bytes_stream());
    sse_response(body)
}

/// 转换：上游 SSE(src) -> 下游 SSE(dst)
///   src = provider 协议, dst = consumer 协议
pub async fn stream_convert(
    resp: reqwest::Response,
    src: Protocol,
    dst: Protocol,
) -> Result<Response, AppError> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(64);
    let mut conv = make_converter(src, dst);

    tokio::spawn(async move {
        use futures::StreamExt;
        let mut buf = BytesMut::new();
        let mut cur_event: Option<String> = None;
        let mut cur_data = String::new();
        let mut stream = resp.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(err = %e, "upstream stream read error");
                    break;
                }
            };
            buf.extend_from_slice(&chunk);

            // 按 \n 切行，遇空行产出一个 SSE 事件
            loop {
                let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                    break;
                };
                let line_bytes = buf.split_to(nl + 1);
                let mut s = String::from_utf8_lossy(&line_bytes).into_owned();
                if s.ends_with('\n') {
                    s.pop();
                }
                if s.ends_with('\r') {
                    s.pop();
                }

                if s.is_empty() {
                    // 事件边界
                    if !cur_data.is_empty() {
                        let payloads = conv.on_event(cur_event.as_deref(), &cur_data);
                        for p in payloads {
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
                    if !cur_data.is_empty() {
                        cur_data.push('\n');
                    }
                    cur_data.push_str(d);
                } else if s.starts_with(':') {
                    // SSE 注释/心跳，忽略
                }
                // 其它未知行忽略
            }
        }

        // 流结束：处理残留事件
        if !cur_data.is_empty() {
            for p in conv.on_event(cur_event.as_deref(), &cur_data) {
                let line = make_sse_line(&p);
                let _ = tx.send(Ok(line.into_bytes().into())).await;
            }
        }
        for p in conv.on_done() {
            let line = make_sse_line(&p);
            let _ = tx.send(Ok(line.into_bytes().into())).await;
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

/// 把一个 payload 包成 SSE 行；[DONE] 也包成 data: [DONE]
fn make_sse_line(payload: &str) -> String {
    format!("data: {payload}\n\n")
}

// ===================== StreamConverter trait =====================

pub trait StreamConverter: Send {
    /// 输入上游一个事件（event 类型 + data payload），输出若干下游 payload
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String>;
    /// 上游结束时收尾
    fn on_done(&mut self) -> Vec<String>;
}

/// 为 Box<dyn StreamConverter> 实现 trait，便于串联
impl StreamConverter for Box<dyn StreamConverter> {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        (**self).on_event(event, data)
    }
    fn on_done(&mut self) -> Vec<String> {
        (**self).on_done()
    }
}

/// 串联两个转换器：A 把上游转成 Chat SSE，B 把 Chat SSE 转成下游
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

/// 不做任何转换（理论上不会走到，透传分支不经过 converter）
struct Noop;
impl StreamConverter for Noop {
    fn on_event(&mut self, _e: Option<&str>, data: &str) -> Vec<String> {
        vec![data.to_string()]
    }
    fn on_done(&mut self) -> Vec<String> {
        vec![]
    }
}

/// 按 (上游协议 src, 下游协议 dst) 选择具体转换器
fn make_converter(src: Protocol, dst: Protocol) -> Box<dyn StreamConverter> {
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
    /// 从 message_start.message.usage 捕获的 input_tokens，
    /// message_delta 的 usage 通常只有 output_tokens，需补入 prompt_tokens
    input_tokens: Option<u64>,
    /// 当前 content block 是否为 thinking 类型
    in_thinking: bool,
    /// 从 message_start 捕获的 chat id，复用到每个 chunk
    chat_id: Option<String>,
    /// 从 message_start 捕获的 model 名
    model: Option<String>,
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
        }
    }

    /// 构造标准 chat.completion.chunk 帧，含 id/object/created/model/logprobs 等标准字段
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
                // 捕获 id / model 供后续帧复用
                if let Some(m) = v.get("message") {
                    if let Some(id) = m.get("id").and_then(|x| x.as_str()) {
                        self.chat_id = Some(id.to_string());
                    }
                    if let Some(model) = m.get("model").and_then(|x| x.as_str()) {
                        self.model = Some(model.to_string());
                    }
                    // 从 message_start 中捕获 input_tokens（message_delta 通常只有 output_tokens）
                    if let Some(u) = m.get("usage") {
                        if let Some(it) = u.get("input_tokens").and_then(|x| x.as_u64()) {
                            self.input_tokens = Some(it);
                        }
                    }
                }
                vec![self.chunk(json!({"role":"assistant","content":""}), None).to_string()]
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
                    "thinking" => {
                        // thinking block 开始：标记状态，等 thinking_delta 产出 reasoning_content
                        self.in_thinking = true;
                        vec![]
                    }
                    _ => {
                        // text block：无需输出，等 delta
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
                        // 思考增量 -> chat 协议的 reasoning_content
                        let text = delta.get("thinking").cloned().unwrap_or(json!(""));
                        vec![self.chunk(json!({"reasoning_content":text}), None).to_string()]
                    }
                    "input_json_delta" => {
                        let pj = delta.get("partial_json").cloned().unwrap_or(json!(""));
                        if let Some(idx) = self.cur_tc_index {
                            vec![self.chunk(json!({
                                "tool_calls":[{"index":idx,"function":{"arguments":pj}}]
                            }), None).to_string()]
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
                // 末帧 delta 含 content 和 reasoning_content（null），与标准 chat chunk 一致
                let mut frame = self.chunk(json!({"content":"","reasoning_content":null}), Some(&finish));
                if let Some(u) = v.get("usage") {
                    let mut usage = json!({});
                    // input_tokens 优先用 message_delta 自带的，否则用 message_start 捕获的
                    let input_tok = u
                        .get("input_tokens")
                        .and_then(|x| x.as_u64())
                        .or(self.input_tokens);
                    if let Some(it) = input_tok {
                        usage["prompt_tokens"] = json!(it);
                    } else if self.input_tokens.is_some() {
                        usage["prompt_tokens"] = json!(self.input_tokens.unwrap());
                    }
                    if let Some(ot) = u.get("output_tokens") {
                        usage["completion_tokens"] = ot.clone();
                    }
                    if let (Some(a), Some(b)) = (
                        input_tok,
                        u.get("output_tokens").and_then(|x| x.as_u64()),
                    ) {
                        usage["total_tokens"] = json!(a + b);
                    }
                    frame["usage"] = usage;
                } else if self.input_tokens.is_some() {
                    // message_delta 无 usage 但有 message_start 捕获的 input_tokens
                    let it = self.input_tokens.unwrap();
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

struct ChatToAnthropicStream {
    started: bool,
    sent_done: bool,
    next_block: usize,
    cur_block: Option<(usize, String)>, // (anthropic block index, type)
}

impl ChatToAnthropicStream {
    fn new() -> Self {
        Self {
            started: false,
            sent_done: false,
            next_block: 0,
            cur_block: None,
        }
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((idx, ref ty)) = self.cur_block {
            if ty == "text" {
                return idx;
            }
            out.push(content_block_stop_frame(idx));
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, "text".into()));
        out.push(content_block_start_text_frame(idx));
        idx
    }
}

impl StreamConverter for ChatToAnthropicStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        if data == "[DONE]" {
            let mut out = vec![];
            if let Some((idx, _)) = self.cur_block.take() {
                out.push(content_block_stop_frame(idx));
            }
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

        // content delta
        if let Some(content) = delta.get("content") {
            if let Some(t) = content.as_str() {
                if !t.is_empty() {
                    let idx = self.ensure_text(&mut out);
                    out.push(text_delta_event(idx, t));
                }
            }
        }

        // tool_calls
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let has_id = tc.get("id").is_some();
                if has_id {
                    if let Some((idx, _)) = self.cur_block.take() {
                        out.push(content_block_stop_frame(idx));
                    }
                    let bidx = self.next_block;
                    self.next_block += 1;
                    self.cur_block = Some((bidx, "tool_use".into()));
                    let id = tc.get("id").cloned().unwrap_or(json!(""));
                    let name = tc["function"]["name"].clone();
                    out.push(content_block_start_tool_event(bidx, id, name));
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        if !args.is_empty() {
                            out.push(input_json_delta_event(bidx, args));
                        }
                    }
                } else if let Some(args) = tc["function"].get("arguments").and_then(|x| x.as_str()) {
                    if let Some((bidx, _)) = &self.cur_block {
                        out.push(input_json_delta_event(*bidx, args));
                    }
                }
            }
        }

        // finish_reason
        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            if let Some((idx, _)) = self.cur_block.take() {
                out.push(content_block_stop_frame(idx));
            }
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
        if let Some((idx, _)) = self.cur_block.take() {
            out.push(content_block_stop_frame(idx));
        }
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
fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}")
}

/// 当前 Unix 时间戳（秒），用于 chat chunk 的 created 字段
fn created_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===================== Responses -> Chat =====================

struct ResponsesToChatStream {
    sent_role: bool,
    sent_done: bool,
    /// output_index -> chat tool_calls index
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
            "response.created" => {
                self.sent_role = true;
                vec![json!({
                    "choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]
                })
                .to_string()]
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(json!({}));
                if item.get("type").and_then(|x| x.as_str()) == Some("function_call") {
                    let oi = v
                        .get("output_index")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
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
                } else {
                    vec![]
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
            "response.function_call_arguments.delta" => {
                let oi = v
                    .get("output_index")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
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
            "response.completed" => {
                let has_tool = !self.tool_map.is_empty();
                let finish = if has_tool { "tool_calls" } else { "stop" };
                let mut frame = json!({
                    "choices":[{"index":0,"delta":{},"finish_reason":finish}]
                });
                if let Some(resp) = v.get("response") {
                    if let Some(u) = resp.get("usage") {
                        let mut usage = json!({});
                        if let Some(it) = u.get("input_tokens") {
                            usage["prompt_tokens"] = it.clone();
                        }
                        if let Some(ot) = u.get("output_tokens") {
                            usage["completion_tokens"] = ot.clone();
                        }
                        if let (Some(a), Some(b)) = (
                            u.get("input_tokens").and_then(|x| x.as_u64()),
                            u.get("output_tokens").and_then(|x| x.as_u64()),
                        ) {
                            usage["total_tokens"] = json!(a + b);
                        }
                        frame["usage"] = usage;
                    }
                }
                self.sent_done = true;
                vec![frame.to_string(), "[DONE]".to_string()]
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

/// 将 chat 协议的 usage（prompt_tokens/completion_tokens/total_tokens）
/// 转换为 responses 协议的 usage（input_tokens/output_tokens/total_tokens），
/// 并确保必要字段存在（codex 解析 response.completed 时要求 input_tokens 存在）
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
    // 缺失字段补 0，避免客户端解析失败
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

struct ChatToResponsesStream {
    created: bool,
    sent_done: bool,
    next_output_index: usize,
    message_oi: Option<usize>,
    /// chat tool index -> responses output_index
    tool_oi: HashMap<usize, usize>,
    usage: Value,
}

impl ChatToResponsesStream {
    fn new() -> Self {
        Self {
            created: false,
            sent_done: false,
            next_output_index: 0,
            message_oi: None,
            tool_oi: HashMap::new(),
            usage: json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}),
        }
    }
}

impl StreamConverter for ChatToResponsesStream {
    fn on_event(&mut self, _event: Option<&str>, data: &str) -> Vec<String> {
        if data == "[DONE]" {
            return vec![]; // 由 finish 帧或 on_done 处理 completed
        }
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out: Vec<String> = vec![];

        if !self.created {
            self.created = true;
            out.push(
                json!({
                    "type":"response.created",
                    "response":{"id":format!("resp_{}",rand_id()),"object":"response","status":"in_progress","output":[]}
                })
                .to_string(),
            );
        }

        let choice = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first());
        let Some(choice) = choice else {
            return out;
        };
        let delta = choice.get("delta").cloned().unwrap_or(json!({}));

        // role 首帧 -> output_item.added(message)
        if delta.get("role").and_then(|x| x.as_str()).is_some() && self.message_oi.is_none() {
            let oi = self.next_output_index;
            self.next_output_index += 1;
            self.message_oi = Some(oi);
            out.push(
                json!({
                    "type":"response.output_item.added","output_index":oi,
                    "item":{"type":"message","role":"assistant","content":[]}
                })
                .to_string(),
            );
        }

        // content delta
        if let Some(content) = delta.get("content").and_then(|x| x.as_str()) {
            if !content.is_empty() {
                let oi = self.message_oi.unwrap_or(0);
                out.push(
                    json!({
                        "type":"response.output_text.delta",
                        "output_index":oi,"content_index":0,"delta":content
                    })
                    .to_string(),
                );
            }
        }

        // tool_calls
        if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
            for tc in tcs {
                let chat_idx = tc
                    .get("index")
                    .and_then(|x| x.as_u64())
                    .map(|i| i as usize)
                    .unwrap_or(0);
                let has_id = tc.get("id").is_some();
                if has_id {
                    let oi = self.next_output_index;
                    self.next_output_index += 1;
                    self.tool_oi.insert(chat_idx, oi);
                    let id = tc.get("id").cloned().unwrap_or(json!(""));
                    let name = tc["function"]["name"].clone();
                    out.push(
                        json!({
                            "type":"response.output_item.added","output_index":oi,
                            "item":{"type":"function_call","call_id":id,"name":name,"arguments":""}
                        })
                        .to_string(),
                    );
                    if let Some(args) = tc["function"]["arguments"].as_str() {
                        if !args.is_empty() {
                            out.push(
                                json!({
                                    "type":"response.function_call_arguments.delta",
                                    "output_index":oi,"delta":args
                                })
                                .to_string(),
                            );
                        }
                    }
                } else if let Some(args) =
                    tc["function"].get("arguments").and_then(|x| x.as_str())
                {
                    if let Some(&oi) = self.tool_oi.get(&chat_idx) {
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

        // finish_reason -> completed
        if let Some(_fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            if let Some(oi) = self.message_oi {
                out.push(
                    json!({
                        "type":"response.output_item.done","output_index":oi,
                        "item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":""}]}
                    })
                    .to_string(),
                );
            }
            if let Some(u) = v.get("usage") {
                self.usage = chat_usage_to_responses_usage(u);
            }
            out.push(
                json!({
                    "type":"response.completed",
                    "response":{"id":format!("resp_{}",rand_id()),"object":"response","status":"completed","output":[],"usage":self.usage.clone()}
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
        vec![json!({
            "type":"response.completed",
            "response":{"id":format!("resp_{}",rand_id()),"object":"response","status":"completed","output":[],"usage":self.usage.clone()}
        })
        .to_string()]
    }
}


