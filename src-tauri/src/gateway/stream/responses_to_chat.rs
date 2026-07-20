//! 流式 Responses → Chat 转换器。
//!
//! 上游发 OpenAI Responses SSE 事件（response.created / response.output_text.delta /
//! response.function_call_arguments.delta / response.completed 等），
//! 转为 OpenAI Chat 流式 chunk（chat.completion.chunk）。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::gateway::stream::pipeline::StreamConverter;

pub(super) struct ResponsesToChatStream {
    sent_role: bool,
    sent_done: bool,
    tool_map: HashMap<u64, usize>,
    next_tc: usize,
}

impl ResponsesToChatStream {
    pub(super) fn new() -> Self {
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
