use std::collections::HashMap;

use serde_json::{json, Value};

use crate::gateway::converter::{map_finish_reason_chat_to_anthropic, rand_id};
use crate::gateway::ir::{BlockKind, ChunkEvent, InternalFinishReason, InternalUsage};

/// Anthropic SSE → IR events。
///
/// 解析 message_start / content_block_start/delta/stop / message_delta / message_stop，
/// 同时处理 thinking_delta / signature_delta / input_json_delta 等子类型。
pub fn parse_anthropic_event(event: Option<&str>, data: &str) -> Vec<ChunkEvent> {
    let _ = event;
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "message_start" => {
            let mut id = String::new();
            let mut model = String::new();
            let mut usage = None;
            if let Some(m) = v.get("message") {
                if let Some(x) = m.get("id").and_then(|x| x.as_str()) {
                    id = x.to_string();
                }
                if let Some(x) = m.get("model").and_then(|x| x.as_str()) {
                    model = x.to_string();
                }
                if let Some(u) = m.get("usage") {
                    let it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
                    let cc = u
                        .get("cache_creation_input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    let cr = u
                        .get("cache_read_input_tokens")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    usage = Some(InternalUsage {
                        input_tokens: it + cc + cr,
                        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                        cache_read_tokens: cr,
                        cache_creation_tokens: cc,
                        reasoning_tokens: 0,
                    });
                }
            }
            vec![ChunkEvent::Start {
                id,
                model,
                role_announced: false,
                usage,
            }]
        }
        "content_block_start" => {
            let idx = v
                .get("index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            let cb = v.get("content_block").cloned().unwrap_or(json!({}));
            let cb_type = cb.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match cb_type {
                "tool_use" => {
                    let id = cb.get("id").cloned().unwrap_or(json!(""));
                    let name = cb.get("name").cloned().unwrap_or(json!(""));
                    vec![
                        ChunkEvent::BlockStart {
                            index: idx,
                            kind: BlockKind::ToolUse,
                        },
                        ChunkEvent::ToolCallStart {
                            upstream_index: idx,
                            id: id_to_string(&id),
                            name: name_to_string(&name),
                        },
                    ]
                }
                "thinking" => {
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::Thinking,
                    }]
                }
                "redacted_thinking" => {
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::RedactedThinking,
                    }]
                }
                "server_tool_use" => {
                    let name = cb.get("name").and_then(|x| x.as_str()).unwrap_or("server_tool").to_string();
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::ServerTool(name),
                    }]
                }
                "web_search_tool_result"
                | "web_fetch_tool_result"
                | "code_execution_tool_result"
                | "bash_code_execution_tool_result"
                | "text_editor_code_execution_tool_result" => {
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::ServerToolResult(cb_type.to_string()),
                    }]
                }
                "fallback" => {
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::Fallback,
                    }]
                }
                _ => {
                    vec![ChunkEvent::BlockStart {
                        index: idx,
                        kind: BlockKind::Text,
                    }]
                }
            }
        }
        "content_block_delta" => {
            let idx = v
                .get("index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            let delta = v.get("delta").cloned().unwrap_or(json!({}));
            let dtype = delta.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match dtype {
                "text_delta" => {
                    let text = delta.get("text").cloned().unwrap_or(json!(""));
                    vec![ChunkEvent::TextDelta(value_to_string(&text))]
                }
                "thinking_delta" => {
                    let text = delta.get("thinking").cloned().unwrap_or(json!(""));
                    vec![ChunkEvent::ReasoningDelta(value_to_string(&text))]
                }
                "signature_delta" => {
                    if let Some(s) = delta.get("signature").and_then(|x| x.as_str()) {
                        if !s.is_empty() {
                            return vec![ChunkEvent::ReasoningSignatureDelta(s.to_string())];
                        }
                    }
                    vec![]
                }
                "input_json_delta" => {
                    let pj = delta.get("partial_json").cloned().unwrap_or(json!(""));
                    vec![ChunkEvent::ToolCallArgsDelta {
                        upstream_index: idx,
                        args: value_to_string(&pj),
                    }]
                }
                _ => vec![],
            }
        }
        "content_block_stop" => {
            let idx = v
                .get("index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            vec![ChunkEvent::BlockStop { index: idx }]
        }
        "message_delta" => {
            let delta = v.get("delta").cloned().unwrap_or(json!({}));
            let stop_reason = delta
                .get("stop_reason")
                .and_then(|x| x.as_str())
                .unwrap_or("end_turn");
            let reason = match stop_reason {
                "end_turn" | "stop_sequence" | "pause_turn" => InternalFinishReason::Stop,
                "tool_use" => InternalFinishReason::ToolCalls,
                "max_tokens" | "model_context_window_exceeded" => InternalFinishReason::Length,
                "refusal" | "unsafe_content" => InternalFinishReason::ContentFilter,
                _ => InternalFinishReason::Stop,
            };
            let usage = v.get("usage").map(parse_anthropic_msg_delta_usage);
            vec![ChunkEvent::Finish { reason, usage }]
        }
        "message_stop" => vec![ChunkEvent::Done],
        _ => vec![],
    }
}

fn parse_anthropic_msg_delta_usage(u: &Value) -> InternalUsage {
    // message_delta 的 usage 只含 output_tokens / input_tokens（增量或全量）
    InternalUsage {
        input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
        reasoning_tokens: 0,
    }
}

// ===================== helper：Value 解包 =====================

fn id_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn name_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn value_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

// ===================== emitters：ChunkEvent → 协议 SSE =====================

/// Anthropic SSE emitter。
///
/// 持有完整的状态机（沿用原 ChatToAnthropicStream 的设计）：
/// - `cur_block: Option<(usize, BlockKind)>`：当前开启的 content_block
/// - `tool_blocks: HashMap<upstream_index, ToolBlockState>`：tool_call 累积状态
///   支持 DeepSeek 乱序到达（id 先到 name 后到）、Copilot 无限空白 bug 中止、late starts 兜底
/// - pending_signature：累积的 reasoning signature_delta，关闭 thinking 块时发
pub struct AnthropicEmitter {
    started: bool,
    sent_done: bool,
    next_block: usize,
    /// (anthropic_index, kind_str)。kind_str 用于 close_cur_block 决定收尾事件
    cur_block: Option<(usize, BlockKind)>,
    pending_signature: Option<String>,
    tool_blocks: HashMap<usize, ToolBlockState>,
    cur_tool_had_delta: bool,
}

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

impl AnthropicEmitter {
    pub fn new() -> Self {
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
    /// 若是 tool_use 块且无任何 input_json_delta，主动补一个 "{}"。
    fn close_cur_block(&mut self, out: &mut Vec<String>) {
        if let Some((idx, kind)) = self.cur_block.take() {
            if kind == BlockKind::Thinking {
                if let Some(sig) = self.pending_signature.take() {
                    out.push(signature_delta_event(idx, &sig));
                }
            }
            if kind == BlockKind::ToolUse && !self.cur_tool_had_delta {
                out.push(input_json_delta_event(idx, "{}"));
            }
            self.cur_tool_had_delta = false;
            out.push(content_block_stop_frame(idx));
        }
    }

    fn ensure_text(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((_, ref kind)) = self.cur_block {
            if *kind == BlockKind::Text {
                return self.cur_block.as_ref().unwrap().0;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, BlockKind::Text));
        out.push(content_block_start_text_frame(idx));
        idx
    }

    fn ensure_thinking(&mut self, out: &mut Vec<String>) -> usize {
        if let Some((_, ref kind)) = self.cur_block {
            if *kind == BlockKind::Thinking {
                return self.cur_block.as_ref().unwrap().0;
            }
            self.close_cur_block(out);
        }
        self.cur_block.take();
        let idx = self.next_block;
        self.next_block += 1;
        self.cur_block = Some((idx, BlockKind::Thinking));
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
        late_starts.sort_unstable_by_key(|(idx, _, _, _)| *idx);
        for (bidx, id, name, pending) in late_starts {
            out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));
            if !pending.is_empty() {
                out.push(input_json_delta_event(bidx, &pending));
            } else {
                out.push(input_json_delta_event(bidx, "{}"));
            }
            out.push(content_block_stop_frame(bidx));
        }
    }

    pub(crate) fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        match ev {
            ChunkEvent::Start { .. } => {
                if !self.started {
                    self.started = true;
                    out.push(message_start_event());
                }
            }
            ChunkEvent::Done => {
                self.close_cur_block(&mut out);
                self.finalize_pending_tool_blocks(&mut out);
                out.push(message_stop_event());
                self.sent_done = true;
                return out;
            }
            ChunkEvent::TextDelta(t) => {
                if !t.is_empty() {
                    let idx = self.ensure_text(&mut out);
                    out.push(text_delta_event(idx, &t));
                }
            }
            ChunkEvent::ReasoningDelta(t) => {
                if !t.is_empty() {
                    let idx = self.ensure_thinking(&mut out);
                    out.push(thinking_delta_event(idx, &t));
                }
            }
            ChunkEvent::ReasoningSignatureDelta(t) => {
                if !t.is_empty() {
                    self.pending_signature = Some(t);
                }
            }
            ChunkEvent::BlockStart { kind, .. } => {
                // 直接来自 Anthropic parser 的 BlockStart 透传（其它 src 协议无此事件），
                // 或来自 Responses parser 的 reasoning item 整块（output_item.added type=reasoning）。
                match kind {
                    BlockKind::Thinking => {
                        // 显式开启 thinking 块（即使无 delta，也要发 content_block_start，
                        // 否则跨协议时多轮 thinking 完整性会丢失）
                        self.ensure_thinking(&mut out);
                    }
                    BlockKind::RedactedThinking => {
                        // redacted_thinking 没有 delta，直接补一条占位 reasoning_content
                        let idx = self.ensure_thinking(&mut out);
                        out.push(thinking_delta_event(idx, "[redacted_thinking]"));
                    }
                    BlockKind::ServerTool(name) => {
                        let idx = self.ensure_text(&mut out);
                        out.push(text_delta_event(idx, &format!("[server_tool_use: {name}]")));
                    }
                    BlockKind::ServerToolResult(name) => {
                        let idx = self.ensure_text(&mut out);
                        out.push(text_delta_event(idx, &format!("[{name}]")));
                    }
                    BlockKind::Fallback => {
                        // text 处理
                    }
                    _ => {}
                }
            }
            ChunkEvent::BlockStop { .. } => {
                self.close_cur_block(&mut out);
            }
            ChunkEvent::ToolCallStart {
                upstream_index,
                id,
                name,
            } => {
                // 累积 id+name 到 state
                {
                    let state = self.tool_blocks.entry(upstream_index).or_default();
                    if !id.is_empty() {
                        state.id = id;
                    }
                    if !name.is_empty() {
                        state.name = name;
                    }
                }
                // 决策：是否宣告（id+name 同时到齐且未宣告）
                let announce_action = {
                    let state = self
                        .tool_blocks
                        .entry(upstream_index)
                        .or_default();
                    if state.aborted {
                        None
                    } else if !state.announced
                        && !state.id.is_empty()
                        && !state.name.is_empty()
                    {
                        let pending = std::mem::take(&mut state.pending_args);
                        Some((state.id.clone(), state.name.clone(), pending))
                    } else {
                        None
                    }
                };
                if let Some((id, name, pending)) = announce_action {
                    self.close_cur_block(&mut out);
                    let bidx = self.next_block;
                    self.next_block += 1;
                    if let Some(state) = self.tool_blocks.get_mut(&upstream_index) {
                        state.anthropic_index = Some(bidx);
                        state.announced = true;
                    }
                    self.cur_block = Some((bidx, BlockKind::ToolUse));
                    self.cur_tool_had_delta = false;
                    out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));
                    if !pending.is_empty() {
                        out.push(input_json_delta_event(bidx, &pending));
                        self.cur_tool_had_delta = true;
                    }
                }
            }
            ChunkEvent::ToolCallArgsDelta { upstream_index, args } => {
                // 累积 args + Copilot 无限空白 bug 检测（在 block 作用域内完成 state 借用）
                let aborted = {
                    let state = self.tool_blocks.entry(upstream_index).or_default();
                    if state.aborted {
                        true
                    } else if !args.is_empty() {
                        for ch in args.chars() {
                            if ch.is_whitespace() {
                                state.consecutive_whitespace += 1;
                            } else {
                                state.consecutive_whitespace = 0;
                            }
                        }
                        if state.consecutive_whitespace >= super::INFINITE_WHITESPACE_THRESHOLD {
                            tracing::warn!(
                                upstream_index,
                                name = %state.name,
                                consecutive_ws = state.consecutive_whitespace,
                                "Copilot 无限空白 bug 检测：中止 tool_call 流"
                            );
                            state.aborted = true;
                            state.pending_args.clear();
                            true
                        } else {
                            state.pending_args.push_str(&args);
                            false
                        }
                    } else {
                        false
                    }
                };
                if aborted {
                    return out;
                }
                // 决策（已释放 state 借用，可自由调用 self 方法）
                let action = {
                    let state = self
                        .tool_blocks
                        .entry(upstream_index)
                        .or_default();
                    if state.aborted {
                        None
                    } else if !state.announced
                        && !state.id.is_empty()
                        && !state.name.is_empty()
                    {
                        // 宣告：flush pending_args
                        let pending = std::mem::take(&mut state.pending_args);
                        Some((true, state.id.clone(), state.name.clone(), pending))
                    } else if state.announced {
                        // 已宣告：发 input_json_delta
                        let args_delta = std::mem::take(&mut state.pending_args);
                        Some((false, String::new(), String::new(), args_delta))
                    } else {
                        None
                    }
                };
                if let Some((is_announce, id, name, payload)) = action {
                    if is_announce {
                        self.close_cur_block(&mut out);
                        let bidx = self.next_block;
                        self.next_block += 1;
                        if let Some(state) = self.tool_blocks.get_mut(&upstream_index) {
                            state.anthropic_index = Some(bidx);
                            state.announced = true;
                        }
                        self.cur_block = Some((bidx, BlockKind::ToolUse));
                        self.cur_tool_had_delta = false;
                        out.push(content_block_start_tool_event(bidx, json!(id), json!(name)));
                        if !payload.is_empty() {
                            out.push(input_json_delta_event(bidx, &payload));
                            self.cur_tool_had_delta = true;
                        }
                    } else if !payload.is_empty() {
                        // 已宣告的 tool_block：发 input_json_delta
                        if let Some(state) = self.tool_blocks.get(&upstream_index) {
                            if let Some(bidx) = state.anthropic_index {
                                out.push(input_json_delta_event(bidx, &payload));
                                self.cur_tool_had_delta = true;
                            }
                        }
                    }
                }
            }
            ChunkEvent::Finish { reason, usage } => {
                self.close_cur_block(&mut out);
                self.finalize_pending_tool_blocks(&mut out);
                // InternalFinishReason → Anthropic stop_reason
                let fr_str = match reason {
                    InternalFinishReason::Stop => "stop",
                    InternalFinishReason::Length => "length",
                    InternalFinishReason::ToolCalls => "tool_calls",
                    InternalFinishReason::ContentFilter => "content_filter",
                };
                let stop_reason = map_finish_reason_chat_to_anthropic(fr_str);
                let mut u = json!({});
                if let Some(usage) = usage {
                    if usage.input_tokens > 0 {
                        u["input_tokens"] = json!(usage.input_tokens);
                    }
                    if usage.output_tokens > 0 {
                        u["output_tokens"] = json!(usage.output_tokens);
                    }
                }
                out.push(message_delta_event(stop_reason, u));
            }
        }
        out
    }

    /// 尝试宣告一个 tool_block（id + name 同时到齐时）。
    ///
    /// 注：宣告决策已内联到 on_event 的 ToolCallStart / ToolCallArgsDelta 分支，
    /// 本方法保留为占位（暂无外部调用）；未来若需要从其它事件触发宣告可复用。
    #[allow(dead_code)]
    fn try_announce_tool(&mut self, _upstream_index: usize, _out: &mut Vec<String>) {}

    pub(crate) fn on_done(&mut self) -> Vec<String> {
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

// ===================== Anthropic SSE 事件构造 helper =====================

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
