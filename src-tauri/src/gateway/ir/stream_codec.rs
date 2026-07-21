//! 流式 SSE chunk 与 IR 的双向映射 + 4 个方向 stream converter。
//!
//! 设计：
//! - parser（`parse_xxx_event`）：上游协议 SSE event/data → `Vec<ChunkEvent>`，无状态纯函数
//! - emitter（`XxxEmitter`）：`ChunkEvent` -> 下游协议 SSE 字符串，**持有状态机**
//!   （AnthropicEmitter 的 ensure_text/thinking/tool_use、Copilot 无限空白检测、
//!   late starts 兜底；ResponsesEmitter 的 reasoning/message item 生命周期等）
//! - `IrStreamConverter`：组合 (parser, emitter)，对外暴露 `StreamConverter` 接口
//!
//! 跨协议双跳（如 Anthropic→Responses）直接走 IR，不再经 Chat 中转。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::config::types::Protocol;
use crate::gateway::ir::helpers::{created_now, map_finish_reason_chat_to_anthropic, rand_id};
use crate::gateway::ir::{BlockKind, ChunkEvent, InternalFinishReason, InternalUsage};
use crate::gateway::stream::StreamConverter;

// Copilot 无限空白 bug 阈值（与原 chat_to_anthropic.rs 保持一致）
const INFINITE_WHITESPACE_THRESHOLD: usize = 500;

// ===================== parsers：协议 SSE → Vec<ChunkEvent> =====================

/// Chat SSE → IR events。
///
/// 解析 chat.completion.chunk 的 choices[0].delta，提取：
/// - delta.content → TextDelta
/// - delta.reasoning_content → ReasoningDelta
/// - delta.reasoning_signature → ReasoningSignatureDelta（累积到 finish 时一起发）
/// - delta.tool_calls[i] 含 id+name → ToolCallStart；含 arguments → ToolCallArgsDelta
/// - choices[0].finish_reason → Finish
/// - "[DONE]" → Done
pub fn parse_chat_event(event: Option<&str>, data: &str) -> Vec<ChunkEvent> {
    let _ = event; // Chat SSE 无 event 字段
    if data == "[DONE]" {
        return vec![ChunkEvent::Done];
    }
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let model = v.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string();

    // usage 可能出现在最后一帧（OpenAI 增量 usage 字段）
    let usage = v.get("usage").map(parse_chat_usage);

    let choice = match v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    {
        Some(c) => c,
        None => return out,
    };
    let delta = choice.get("delta").cloned().unwrap_or(json!({}));

    // role 首帧（"role":"assistant"）→ Start
    if let Some(role) = delta.get("role").and_then(|x| x.as_str()) {
        if role == "assistant" {
            out.push(ChunkEvent::Start {
                id,
                model,
                role_announced: true,
                usage: None,
            });
        }
    }

    // signature（chat 端的 reasoning_signature 增量；stream 中通常在 finish 帧前累积）
    if let Some(sig) = delta.get("reasoning_signature").and_then(|x| x.as_str()) {
        if !sig.is_empty() {
            out.push(ChunkEvent::ReasoningSignatureDelta(sig.to_string()));
        }
    }

    if let Some(rc) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
        if !rc.is_empty() {
            out.push(ChunkEvent::ReasoningDelta(rc.to_string()));
        }
    }

    if let Some(content) = delta.get("content").and_then(|x| x.as_str()) {
        if !content.is_empty() {
            out.push(ChunkEvent::TextDelta(content.to_string()));
        }
    }

    if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
        for tc in tcs {
            let upstream_index = tc
                .get("index")
                .and_then(|i| i.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            // 含 id 或 name 任一 -> ToolCallStart（emitter 负责"id+name 都到齐才宣告"，
            // 兼容 DeepSeek/GLM 乱序到达：id 先到 name 后到）
            let has_id = tc.get("id").is_some();
            let has_name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .is_some();
            if has_id || has_name {
                let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                out.push(ChunkEvent::ToolCallStart {
                    upstream_index,
                    id,
                    name,
                });
            }
            if let Some(args) = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
            {
                if !args.is_empty() {
                    out.push(ChunkEvent::ToolCallArgsDelta {
                        upstream_index,
                        args: args.to_string(),
                    });
                }
            }
        }
    }

    if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
        let reason = match fr {
            "stop" => InternalFinishReason::Stop,
            "length" => InternalFinishReason::Length,
            "tool_calls" | "function_call" => InternalFinishReason::ToolCalls,
            "content_filter" => InternalFinishReason::ContentFilter,
            _ => InternalFinishReason::Stop,
        };
        out.push(ChunkEvent::Finish { reason, usage });
    }
    out
}

fn parse_chat_usage(u: &Value) -> InternalUsage {
    InternalUsage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: u
            .get("cache_read_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        cache_creation_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        reasoning_tokens: 0,
    }
}

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

/// Responses SSE → IR events。
///
/// 解析 response.created / response.output_text.delta / response.reasoning_summary_text.delta /
/// response.function_call_arguments.delta / response.output_item.added / response.completed 等。
pub fn parse_responses_event(event: Option<&str>, data: &str) -> Vec<ChunkEvent> {
    let _ = event;
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return vec![],
    };
    let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "response.created" | "response.in_progress" => {
            let id = v
                .pointer("/response/id")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let model = v
                .pointer("/response/model")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            vec![ChunkEvent::Start {
                id,
                model,
                role_announced: false,
                usage: None,
            }]
        }
        "response.output_item.added" => {
            let item = v.get("item").cloned().unwrap_or(json!({}));
            let upstream_index = v
                .get("output_index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            let itype = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match itype {
                "function_call" => {
                    let id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    vec![ChunkEvent::ToolCallStart {
                        upstream_index,
                        id,
                        name,
                    }]
                }
                // reasoning item：OpenAI 标准 Responses 的两种 reasoning 输出模式之一
                // —— encrypted 模式（另一个是 summary 模式，走 reasoning_summary_text.delta）。
                // encrypted 模式下，provider 仅通过 encrypted_content（加密 blob）传思考过程，
                // 不发明文 delta。这里展开为 BlockStart(Thinking) +
                // ReasoningDelta(若 summary 非空) + ReasoningSignatureDelta(encrypted_content)。
                "reasoning" => {
                    let mut out = vec![ChunkEvent::BlockStart {
                        index: upstream_index,
                        kind: BlockKind::Thinking,
                    }];
                    // summary 数组中的 summary_text -> ReasoningDelta（明文思考增量）
                    let summary_text = item
                        .get("summary")
                        .and_then(|s| s.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|p| {
                                    if p.get("type").and_then(|t| t.as_str())
                                        == Some("summary_text")
                                    {
                                        p.get("text").and_then(|t| t.as_str()).map(String::from)
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    if !summary_text.is_empty() {
                        out.push(ChunkEvent::ReasoningDelta(summary_text));
                    }
                    // content 数组中的 reasoning_text -> ReasoningDelta（部分 provider 走此字段）
                    let content_text = item
                        .get("content")
                        .and_then(|c| c.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|p| {
                                    if p.get("type").and_then(|t| t.as_str()) == Some("reasoning_text")
                                    {
                                        p.get("text").and_then(|t| t.as_str()).map(String::from)
                                    } else {
                                        None
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("")
                        })
                        .unwrap_or_default();
                    if !content_text.is_empty() {
                        out.push(ChunkEvent::ReasoningDelta(content_text));
                    }
                    // encrypted_content -> ReasoningSignatureDelta（多轮 thinking 完整性）
                    if let Some(enc) =
                        item.get("encrypted_content").and_then(|x| x.as_str())
                    {
                        if !enc.is_empty() {
                            out.push(ChunkEvent::ReasoningSignatureDelta(enc.to_string()));
                        }
                    }
                    out
                }
                // 服务端工具调用 item（web_search/file_search/code_interpreter 等）转文本说明
                "web_search_call"
                | "file_search_call"
                | "image_generation_call"
                | "code_interpreter_call"
                | "computer_call"
                | "mcp_call" => {
                    let label = match itype {
                        "web_search_call" => "web_search",
                        "file_search_call" => "file_search",
                        "image_generation_call" => "image_generation",
                        "code_interpreter_call" => "code_interpreter",
                        "computer_call" => "computer",
                        _ => "mcp",
                    };
                    vec![ChunkEvent::TextDelta(format!("[{label}]"))]
                }
                _ => vec![],
            }
        }
        "response.output_item.done" => {
            // reasoning item 的关闭信号：触发 BlockStop，让 Anthropic/Chat 客户端关闭 thinking 块
            let item = v.get("item").cloned().unwrap_or(json!({}));
            let upstream_index = v
                .get("output_index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            let itype = item.get("type").and_then(|x| x.as_str()).unwrap_or("");
            if itype == "reasoning" {
                vec![ChunkEvent::BlockStop { index: upstream_index }]
            } else {
                vec![]
            }
        }
        "response.output_text.delta" => {
            let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
            if d.is_empty() {
                vec![]
            } else {
                vec![ChunkEvent::TextDelta(d.to_string())]
            }
        }
        "response.refusal.delta" => {
            let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
            if d.is_empty() {
                vec![]
            } else {
                vec![ChunkEvent::TextDelta(d.to_string())]
            }
        }
        // 推理增量（多种事件名兼容）
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
                vec![ChunkEvent::ReasoningDelta(d)]
            }
        }
        "response.function_call_arguments.delta" => {
            let upstream_index = v
                .get("output_index")
                .and_then(|x| x.as_u64())
                .map(|i| i as usize)
                .unwrap_or(0);
            let d = v.get("delta").and_then(|x| x.as_str()).unwrap_or("");
            if d.is_empty() {
                vec![]
            } else {
                vec![ChunkEvent::ToolCallArgsDelta {
                    upstream_index,
                    args: d.to_string(),
                }]
            }
        }
        "response.completed"
        | "response.incomplete"
        | "response.failed"
        | "response.cancelled" => {
            let has_tool = if let Some(arr) = v
                .pointer("/response/output")
                .and_then(|x| x.as_array())
            {
                arr.iter().any(|i| {
                    i.get("type").and_then(|x| x.as_str()) == Some("function_call")
                })
            } else {
                false
            };
            let reason = if t == "response.failed" || t == "response.cancelled" {
                InternalFinishReason::Stop
            } else if has_tool {
                InternalFinishReason::ToolCalls
            } else if t == "response.incomplete" {
                InternalFinishReason::Length
            } else {
                InternalFinishReason::Stop
            };
            let usage = v
                .pointer("/response/usage")
                .map(parse_responses_completion_usage);
            vec![
                ChunkEvent::Finish { reason, usage },
                ChunkEvent::Done,
            ]
        }
        // 其它终止/分部事件，转 IR 时忽略
        _ => vec![],
    }
}

fn parse_responses_completion_usage(u: &Value) -> InternalUsage {
    let it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let cc = u
        .get("cache_creation_input_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    let cr = u
        .get("cache_read_input_tokens")
        .and_then(|x| x.as_u64())
        .unwrap_or(0);
    InternalUsage {
        input_tokens: it + cc + cr,
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: cr,
        cache_creation_tokens: cc,
        reasoning_tokens: 0,
    }
}

// ===================== emitters：ChunkEvent → 协议 SSE =====================

fn id_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn name_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}
fn value_to_string(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// Chat SSE emitter。
///
/// Chat 协议流式 chunk 结构相对扁平（choices[0].delta），不需要复杂状态机。
/// 主要职责：
/// - Start 事件 → 发首帧 delta:{role:"assistant"}
/// - TextDelta → delta:{content}
/// - ReasoningDelta → delta:{reasoning_content}
/// - ReasoningSignatureDelta → 累积，等 Finish 一起发
/// - ToolCallStart → delta:{tool_calls:[{index,id,type: function,function:{name,arguments:""}}]}
/// - ToolCallArgsDelta → delta:{tool_calls:[{index,function:{arguments}}]}
/// - Finish → 发 finish_reason + reasoning_signature + usage
/// - Done → 发 "[DONE]"
pub struct ChatEmitter {
    chat_id: String,
    model: String,
    sent_role: bool,
    sent_done: bool,
    /// 累积 reasoning_signature，Finish 帧一次性发
    pending_signatures: Vec<String>,
}

impl ChatEmitter {
    pub fn new() -> Self {
        Self {
            chat_id: String::new(),
            model: String::new(),
            sent_role: false,
            sent_done: false,
            pending_signatures: Vec::new(),
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        let id = if self.chat_id.is_empty() {
            format!("chatcmpl-{}", rand_id())
        } else {
            self.chat_id.clone()
        };
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created_now(),
            "model": self.model,
            "choices":[{
                "index": 0,
                "delta": delta,
                "logprobs": null,
                "finish_reason": finish
            }]
        })
    }

    fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
        match ev {
            ChunkEvent::Start {
                id,
                model,
                role_announced,
                ..
            } => {
                if !id.is_empty() {
                    self.chat_id = id;
                }
                if !model.is_empty() {
                    self.model = model;
                }
                if role_announced || !self.sent_role {
                    self.sent_role = true;
                    vec![self
                        .chunk(json!({"role":"assistant","content":""}), None)
                        .to_string()]
                } else {
                    vec![]
                }
            }
            ChunkEvent::Done => {
                self.sent_done = true;
                vec!["[DONE]".to_string()]
            }
            ChunkEvent::TextDelta(t) => {
                vec![self.chunk(json!({"content":t}), None).to_string()]
            }
            ChunkEvent::ReasoningDelta(t) => {
                vec![self.chunk(json!({"reasoning_content":t}), None).to_string()]
            }
            ChunkEvent::ReasoningSignatureDelta(t) => {
                self.pending_signatures.push(t);
                vec![]
            }
            ChunkEvent::ToolCallStart {
                upstream_index,
                id,
                name,
            } => {
                vec![self.chunk(json!({
                    "tool_calls":[{"index":upstream_index,"id":id,"type":"function","function":{"name":name,"arguments":""}}]
                }), None).to_string()]
            }
            ChunkEvent::ToolCallArgsDelta { upstream_index, args } => {
                vec![self.chunk(json!({
                    "tool_calls":[{"index":upstream_index,"function":{"arguments":args}}]
                }), None).to_string()]
            }
            ChunkEvent::Finish { reason, usage } => {
                let fr = match reason {
                    InternalFinishReason::Stop => "stop",
                    InternalFinishReason::Length => "length",
                    InternalFinishReason::ToolCalls => "tool_calls",
                    InternalFinishReason::ContentFilter => "content_filter",
                };
                let mut delta = json!({});
                if !self.pending_signatures.is_empty() {
                    delta["reasoning_signature"] =
                        json!(std::mem::take(&mut self.pending_signatures).join("\n"));
                }
                let mut frame = self.chunk(delta, Some(fr));
                if let Some(u) = usage {
                    let mut out = json!({});
                    if u.input_tokens > 0 {
                        out["prompt_tokens"] = json!(u.input_tokens);
                    }
                    if u.output_tokens > 0 {
                        out["completion_tokens"] = json!(u.output_tokens);
                    }
                    if u.input_tokens > 0 || u.output_tokens > 0 {
                        out["total_tokens"] = json!(u.input_tokens + u.output_tokens);
                    }
                    if u.cache_creation_tokens > 0 {
                        out["cache_creation_input_tokens"] = json!(u.cache_creation_tokens);
                    }
                    if u.cache_read_tokens > 0 {
                        out["cache_read_input_tokens"] = json!(u.cache_read_tokens);
                    }
                    frame["usage"] = out;
                }
                vec![frame.to_string()]
            }
            // Anthropic/Responses 的 BlockStart/BlockStop 在 Chat 中无概念，跳过
            ChunkEvent::BlockStart { .. } | ChunkEvent::BlockStop { .. } => vec![],
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

    fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
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
                        if state.consecutive_whitespace >= INFINITE_WHITESPACE_THRESHOLD {
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

// Anthropic SSE 事件构造 helper
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

/// Responses SSE emitter。
///
/// 持有 reasoning/message/function_call item 生命周期状态机（沿用原 ChatToResponsesStream）：
/// - 任何时刻只能有一个未关闭的 message 或 reasoning item
/// - tool_call 不与 message 并存
/// - response.completed 时回填所有 item 的完整内容到 output 数组
pub struct ResponsesEmitter {
    resp_id: String,
    msg_item_id: String,
    reasoning_item_id: String,
    created: bool,
    sent_done: bool,
    next_output_index: usize,
    message_oi: Option<usize>,
    reasoning_oi: Option<usize>,
    tool_oi: HashMap<usize, usize>,
    tool_items: HashMap<usize, ToolCallState>,
    usage: Value,
    accumulated_text: String,
    accumulated_reasoning: String,
    message_item_added: bool,
    reasoning_item_added: bool,
    final_output: Vec<Value>,
}

struct ToolCallState {
    item_id: String,
    call_id: Value,
    name: Value,
    arguments: String,
}

impl ResponsesEmitter {
    pub fn new() -> Self {
        Self {
            resp_id: format!("resp_{}", rand_id()),
            msg_item_id: format!("msg_{}", rand_id()),
            reasoning_item_id: format!("rs_{}", rand_id()),
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

    fn close_all(&mut self, out: &mut Vec<String>) {
        self.close_message(out);
        self.close_reasoning(out);
        self.close_tool_calls(out);
    }

    fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        match ev {
            ChunkEvent::Start { id, .. } => {
                self.ensure_created(&mut out);
                if !id.is_empty() {
                    self.resp_id = id;
                }
            }
            ChunkEvent::Done => {
                // Responses 协议中 Done 已由 Finish 触发（response.completed 后立即 Done）；
                // 这里防御性确保 sent_done
                self.sent_done = true;
            }
            ChunkEvent::TextDelta(t) => {
                self.ensure_created(&mut out);
                if !t.is_empty() {
                    self.ensure_message_item(&mut out);
                    self.accumulated_text.push_str(&t);
                    out.push(
                        json!({
                            "type":"response.output_text.delta",
                            "item_id":self.msg_item_id,
                            "output_index":self.message_oi.unwrap_or(0),
                            "content_index":0,
                            "delta":t
                        })
                        .to_string(),
                    );
                }
            }
            ChunkEvent::ReasoningDelta(t) => {
                self.ensure_created(&mut out);
                if !t.is_empty() {
                    self.ensure_reasoning_item(&mut out);
                    self.accumulated_reasoning.push_str(&t);
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
            ChunkEvent::ReasoningSignatureDelta(_) => {
                // Responses 协议无对应 reasoning signature delta 事件，忽略
                // encrypted_content 仅在最终 reasoning item 中体现（非流式场景）
            }
            ChunkEvent::BlockStart { .. } | ChunkEvent::BlockStop { .. } => {
                // Chat 端不发，Anthropic 端的发由具体 delta 事件驱动
            }
            ChunkEvent::ToolCallStart {
                upstream_index,
                id,
                name,
            } => {
                self.ensure_created(&mut out);
                self.close_message(&mut out);
                self.close_reasoning(&mut out);
                let oi = self.next_output_index;
                self.next_output_index += 1;
                self.tool_oi.insert(upstream_index, oi);
                let call_id_val = json!(id);
                let name_val = json!(name);
                let item_id = format!("fc_{}", rand_id());
                out.push(
                    json!({
                        "type":"response.output_item.added","output_index":oi,
                        "item":{
                            "id":item_id,
                            "type":"function_call",
                            "call_id":id,
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
                        call_id: call_id_val,
                        name: name_val,
                        arguments: String::new(),
                    },
                );
            }
            ChunkEvent::ToolCallArgsDelta { upstream_index, args } => {
                self.ensure_created(&mut out);
                if let Some(&oi) = self.tool_oi.get(&upstream_index) {
                    if let Some(st) = self.tool_items.get_mut(&oi) {
                        st.arguments.push_str(&args);
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
            ChunkEvent::Finish { reason, usage } => {
                self.ensure_created(&mut out);
                self.close_all(&mut out);
                if let Some(u) = usage {
                    let mut out_u = json!({});
                    if u.input_tokens > 0 {
                        out_u["input_tokens"] = json!(u.input_tokens);
                    }
                    if u.output_tokens > 0 {
                        out_u["output_tokens"] = json!(u.output_tokens);
                    }
                    if u.input_tokens > 0 || u.output_tokens > 0 {
                        out_u["total_tokens"] = json!(u.input_tokens + u.output_tokens);
                    } else {
                        out_u["total_tokens"] = json!(0);
                    }
                    if out_u.get("input_tokens").is_none() {
                        out_u["input_tokens"] = json!(0);
                    }
                    if out_u.get("output_tokens").is_none() {
                        out_u["output_tokens"] = json!(0);
                    }
                    self.usage = out_u;
                }
                let _ = reason; // Responses 协议的 status 由 close_all + completed 隐含
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

// ===================== IrStreamConverter：parser + emitter 组合 =====================

enum AnyEmitter {
    Chat(Box<ChatEmitter>),
    Anthropic(Box<AnthropicEmitter>),
    Responses(Box<ResponsesEmitter>),
}

/// 通用 IR 流转换器：根据 src 协议选 parser，根据 dst 协议选 emitter。
///
/// 替代原 4 个独立 stream converter + Chained 组合器。
/// 跨协议双跳（如 Anthropic→Responses）直接走 IR，不再经 Chat 中转。
pub struct IrStreamConverter {
    src: Protocol,
    emitter: AnyEmitter,
}

impl IrStreamConverter {
    pub fn new(src: Protocol, dst: Protocol) -> Self {
        let emitter = match dst {
            Protocol::Chat => AnyEmitter::Chat(Box::new(ChatEmitter::new())),
            Protocol::Anthropic => AnyEmitter::Anthropic(Box::new(AnthropicEmitter::new())),
            Protocol::Responses => AnyEmitter::Responses(Box::new(ResponsesEmitter::new())),
        };
        Self { src, emitter }
    }

    fn parse(&self, event: Option<&str>, data: &str) -> Vec<ChunkEvent> {
        match self.src {
            Protocol::Chat => parse_chat_event(event, data),
            Protocol::Anthropic => parse_anthropic_event(event, data),
            Protocol::Responses => parse_responses_event(event, data),
        }
    }
}

impl StreamConverter for IrStreamConverter {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        let events = self.parse(event, data);
        let mut out = Vec::new();
        for ev in events {
            let payloads = match &mut self.emitter {
                AnyEmitter::Chat(e) => e.on_event(ev),
                AnyEmitter::Anthropic(e) => e.on_event(ev),
                AnyEmitter::Responses(e) => e.on_event(ev),
            };
            out.extend(payloads);
        }
        out
    }

    fn on_done(&mut self) -> Vec<String> {
        match &mut self.emitter {
            AnyEmitter::Chat(e) => e.on_done(),
            AnyEmitter::Anthropic(e) => e.on_done(),
            AnyEmitter::Responses(e) => e.on_done(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::Protocol;
    use crate::gateway::stream::StreamConverter;

    /// 验证：上游 Responses 以 encrypted 模式发 reasoning（output_item.added/done 含
    /// encrypted_content，无 summary_text delta）时，转换到 Anthropic 客户端能正确产生
    /// content_block_start(thinking) + signature_delta + content_block_stop，
    /// 保证多轮 thinking 完整性。
    ///
    /// 这是 OpenAI 标准 Responses 协议定义的两种 reasoning 输出形态之一。
    #[test]
    fn responses_to_anthropic_reasoning_item_block_translates_to_thinking_block() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Anthropic);

        // 1) response.created
        let created = r#"{"type":"response.created","response":{"id":"resp_1","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        // 2) response.output_item.added (type=reasoning, summary=[], encrypted_content="blob_x")
        let added = r#"{"type":"response.output_item.added","output_index":0,"sequence_number":2,"item":{"id":"rs_1","type":"reasoning","content":[],"encrypted_content":"blob_x","summary":[]}}"#;
        let out_added = conv.on_event(None, added);
        let combined_added: String = out_added.iter().cloned().collect();
        // 应当开启 thinking 块（content_block_start + type=thinking）
        assert!(
            combined_added.contains("content_block_start") && combined_added.contains("\"thinking\""),
            "reasoning item added should open thinking block, got: {combined_added}"
        );

        // 3) response.output_item.done (type=reasoning) —— 关闭 thinking 块，
        //    关闭前应 flush encrypted_content 作为 signature_delta
        let done = r#"{"type":"response.output_item.done","output_index":0,"sequence_number":3,"item":{"id":"rs_1","type":"reasoning","content":[],"encrypted_content":"blob_x","summary":[]}}"#;
        let out_done = conv.on_event(None, done);
        let combined_done: String = out_done.iter().cloned().collect();
        assert!(
            combined_done.contains("signature_delta") && combined_done.contains("blob_x"),
            "thinking block close should flush encrypted_content as signature_delta, got: {combined_done}"
        );
        assert!(
            combined_done.contains("content_block_stop"),
            "reasoning item done should close thinking block, got: {combined_done}"
        );
    }

    /// 验证：当 reasoning item 的 summary 含明文 summary_text 时，转换到 Anthropic
    /// 会作为 thinking_delta 输出（而非仅作为加密块）。
    #[test]
    fn responses_to_anthropic_reasoning_with_summary_text_emits_thinking_delta() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Anthropic);

        let created = r#"{"type":"response.created","response":{"id":"resp_2","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_2","type":"reasoning","summary":[{"type":"summary_text","text":"I should answer."}],"encrypted_content":"sig_abc"}}"#;
        let out = conv.on_event(None, added);
        let combined: String = out.iter().cloned().collect();
        assert!(
            combined.contains("thinking_delta") && combined.contains("I should answer."),
            "summary_text should be emitted as thinking_delta, got: {combined}"
        );
    }

    /// 验证：转换到 Chat 客户端时，reasoning item 的 encrypted_content 作为
    /// reasoning_signature 在 Finish 帧透传（Chat 协议无"块"概念，依赖 Finish 累积）。
    #[test]
    fn responses_to_chat_reasoning_item_signature_passed_in_finish_frame() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Chat);

        let created = r#"{"type":"response.created","response":{"id":"resp_3","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_3","type":"reasoning","summary":[],"encrypted_content":"blob_y"}}"#;
        let _ = conv.on_event(None, added);

        let done = r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_3","type":"reasoning","summary":[],"encrypted_content":"blob_y"}}"#;
        let _ = conv.on_event(None, done);

        // response.completed 触发 Finish + Done
        let completed = r#"{"type":"response.completed","response":{"id":"resp_3","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let out = conv.on_event(None, completed);
        let combined: String = out.iter().cloned().collect();
        assert!(
            combined.contains("reasoning_signature") && combined.contains("blob_y"),
            "encrypted_content should be flushed as reasoning_signature in finish frame, got: {combined}"
        );
    }
}
