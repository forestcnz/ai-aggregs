use serde_json::{json, Value};

use crate::gateway::converter::{created_now, rand_id};
use crate::gateway::ir::{ChunkEvent, InternalFinishReason, InternalUsage};

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

// ===================== emitters：ChunkEvent → 协议 SSE =====================

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

    pub(crate) fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
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

    pub(crate) fn on_done(&mut self) -> Vec<String> {
        if self.sent_done {
            vec![]
        } else {
            self.sent_done = true;
            vec!["[DONE]".to_string()]
        }
    }
}
