//! 流式 Chat → Responses 转换器。
//!
//! 上游发 OpenAI Chat 流式 chunk（含 delta.content / delta.tool_calls / finish_reason），
//! 转为 OpenAI Responses SSE 事件（response.created / response.output_text.delta /
//! response.function_call_arguments.delta / response.completed 等）。
//!
//! 内部维护 output item 生命周期（reasoning / message / function_call），
//! 关闭时回填完整内容到 `response.completed.output`。

use std::collections::HashMap;

use serde_json::{json, Value};

use crate::gateway::converter::{created_now, rand_id};
use crate::gateway::stream::pipeline::StreamConverter;

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

pub(super) struct ChatToResponsesStream {
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
    pub(super) fn new() -> Self {
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
