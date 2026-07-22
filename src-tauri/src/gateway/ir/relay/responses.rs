use std::collections::HashMap;

use serde_json::{json, Value};

use crate::gateway::converter::{created_now, rand_id};
use crate::gateway::ir::{BlockKind, ChunkEvent, InternalFinishReason, InternalUsage};

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

    pub(crate) fn on_event(&mut self, ev: ChunkEvent) -> Vec<String> {
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

    pub(crate) fn on_done(&mut self) -> Vec<String> {
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
