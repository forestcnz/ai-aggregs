//! 流式 Anthropic → Chat 转换器。
//!
//! 上游发 Anthropic SSE 事件（message_start / content_block_* / message_delta / message_stop），
//! 转为 OpenAI Chat 流式 chunk（chat.completion.chunk）。

use serde_json::{json, Value};

use crate::gateway::converter::{created_now, map_stop_reason_anthropic_to_chat, rand_id};
use crate::gateway::stream::pipeline::StreamConverter;

pub(super) struct AnthropicToChatStream {
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
    pub(super) fn new() -> Self {
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
