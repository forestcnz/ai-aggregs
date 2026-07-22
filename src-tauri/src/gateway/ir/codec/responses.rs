use serde_json::{json, Map, Value};

use crate::error::AppError;
use crate::gateway::convert_helpers::{strip_cache_control, strip_leading_anthropic_billing_header};
use crate::gateway::converter::rand_id;
use crate::gateway::ir::{
    InternalContent, InternalFinishReason, InternalMessage, InternalReasoning,
    InternalReasoningBlock, InternalRequest, InternalResponse, InternalRole, InternalTool,
    InternalToolCall, InternalToolChoice, InternalToolKind, InternalUsage,
};
use crate::gateway::reasoning_bridge;
use crate::gateway::tools as codex_tools;

// ===================== parse：协议 JSON -> IR =====================

/// 把 Responses 协议请求体解析为 IR。
///
/// parse 阶段做的事：
/// - instructions 字段 -> IR.system（剥离 billing header）
/// - input 数组（message / reasoning / function_call / function_call_output）映射到 IR.messages
///   （reasoning item 用 reasoning_bridge 编码为 envelope 放进 reasoning.signature）
/// - tools 数组中立化：function 直传；namespace/custom/tool_search 摊平
pub fn parse_responses_req(src: &Value) -> Result<InternalRequest, AppError> {
    let mut messages = Vec::new();
    let mut system_text = String::new();

    if let Some(ins) = src.get("instructions").and_then(|x| x.as_str()) {
        let ins = strip_leading_anthropic_billing_header(ins);
        system_text = ins.to_string();
    }

    match src.get("input") {
        Some(Value::String(s)) => {
            messages.push(InternalMessage {
                role: InternalRole::User,
                content: vec![InternalContent::Text { text: s.clone() }],
                tool_calls: vec![],
                tool_call_id: None,
                reasoning: None,
            });
        }
        Some(Value::Array(arr)) => {
            for item in arr {
                let itype = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match itype {
                    "message" => {
                        let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let ir_role = match role {
                            "user" => InternalRole::User,
                            "assistant" => InternalRole::Assistant,
                            "system" => InternalRole::System,
                            _ => InternalRole::User,
                        };
                        let text = super::responses_content_to_text(item.get("content"), role);
                        if !text.is_empty() {
                            messages.push(InternalMessage {
                                role: ir_role,
                                content: vec![InternalContent::Text { text }],
                                tool_calls: vec![],
                                tool_call_id: None,
                                reasoning: None,
                            });
                        }
                    }
                    "reasoning" => {
                        let summary_text = item
                            .get("summary")
                            .and_then(|s| s.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|p| {
                                        if p.get("type").and_then(|t| t.as_str()) == Some("summary_text") {
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
                            let signature = reasoning_bridge::encode_openai_reasoning(item);
                            messages.push(InternalMessage {
                                role: InternalRole::Assistant,
                                content: vec![],
                                tool_calls: vec![],
                                tool_call_id: None,
                                reasoning: Some(InternalReasoningBlock {
                                    thinking: summary_text,
                                    signature,
                                    redacted: false,
                                }),
                            });
                        }
                    }
                    "function_call" => {
                        let id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let args = item.get("arguments").and_then(|x| x.as_str()).unwrap_or("{}").to_string();
                        let pushed = if let Some(last) = messages.last_mut() {
                            if last.role == InternalRole::Assistant && last.tool_call_id.is_none() {
                                last.tool_calls.push(InternalToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: args.clone(),
                                    namespace: None,
                                    custom_input: None,
                                });
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if !pushed {
                            messages.push(InternalMessage {
                                role: InternalRole::Assistant,
                                content: vec![],
                                tool_calls: vec![InternalToolCall {
                                    id,
                                    name,
                                    arguments: args,
                                    namespace: None,
                                    custom_input: None,
                                }],
                                tool_call_id: None,
                                reasoning: None,
                            });
                        }
                    }
                    "function_call_output" => {
                        let call_id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        let output = item.get("output").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        messages.push(InternalMessage {
                            role: InternalRole::Tool,
                            content: vec![InternalContent::Text { text: output }],
                            tool_calls: vec![],
                            tool_call_id: Some(call_id),
                            reasoning: None,
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let mut tools = Vec::new();
    if let Some(arr) = src.get("tools").and_then(|t| t.as_array()) {
        let mut tool_search_seen = false;
        for t in arr {
            let tool_type = t.get("type").and_then(|x| x.as_str()).unwrap_or("function");
            match tool_type {
                "function" => {
                    tools.push(InternalTool {
                        name: t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                        description: t.get("description").and_then(|x| x.as_str()).map(String::from),
                        parameters: t.get("parameters").cloned().unwrap_or(json!({})),
                        strict: false,
                        kind: InternalToolKind::Function,
                    });
                }
                "namespace" => {
                    let ns_name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let children: Option<&Vec<Value>> = t
                        .get("tools")
                        .and_then(|t| t.as_array())
                        .or_else(|| t.get("children").and_then(|c| c.as_array()));
                    if let Some(children) = children {
                        for (flat, _owner) in codex_tools::flatten_namespace_children(ns_name, children) {
                            if let Some(f) = flat.get("function") {
                                tools.push(InternalTool {
                                    name: f.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                    description: f.get("description").and_then(|x| x.as_str()).map(String::from),
                                    parameters: f.get("parameters").cloned().unwrap_or(json!({})),
                                    strict: false,
                                    kind: InternalToolKind::Function,
                                });
                            }
                        }
                    }
                }
                "custom" | "freeform" => {
                    let flat = codex_tools::custom_tool_to_function(t);
                    if let Some(f) = flat.get("function") {
                        tools.push(InternalTool {
                            name: f.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            description: f.get("description").and_then(|x| x.as_str()).map(String::from),
                            parameters: f.get("parameters").cloned().unwrap_or(json!({})),
                            strict: false,
                            kind: InternalToolKind::Function,
                        });
                    }
                }
                "tool_search" => {
                    if !tool_search_seen {
                        let proxy = codex_tools::tool_search_proxy_function();
                        if let Some(f) = proxy.get("function") {
                            tools.push(InternalTool {
                                name: f.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                                description: f.get("description").and_then(|x| x.as_str()).map(String::from),
                                parameters: f.get("parameters").cloned().unwrap_or(json!({})),
                                strict: false,
                                kind: InternalToolKind::Function,
                            });
                        }
                        tool_search_seen = true;
                    }
                }
                _ => {}
            }
        }
    }

    let tool_choice = src.get("tool_choice").map(parse_responses_tool_choice);

    let mut reasoning = None;
    if let Some(effort) = src.pointer("/reasoning/effort").and_then(|v| v.as_str()) {
        if !effort.is_empty() {
            let summary = src
                .pointer("/reasoning/summary")
                .and_then(|v| v.as_str())
                .map(String::from);
            reasoning = Some(InternalReasoning {
                effort: Some(effort.to_string()),
                summary,
                ..InternalReasoning::default()
            });
        }
    }

    let mut extensions = Map::new();
    for key in ["previous_response_id", "store", "metadata"] {
        if let Some(v) = src.get(key) {
            extensions.insert(key.into(), v.clone());
        }
    }

    Ok(InternalRequest {
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        system: if system_text.is_empty() { None } else { Some(system_text) },
        messages,
        tools,
        tool_choice,
        max_tokens: src.get("max_output_tokens").and_then(|x| x.as_u64()).map(|v| v as u32),
        temperature: src.get("temperature").and_then(|x| x.as_f64()).map(|v| v as f32),
        top_p: src.get("top_p").and_then(|x| x.as_f64()).map(|v| v as f32),
        stop: Vec::new(),
        stream: src.get("stream").and_then(|x| x.as_bool()).unwrap_or(false),
        reasoning,
        parallel_tool_calls: src.get("parallel_tool_calls").and_then(|x| x.as_bool()),
        extensions,
    })
}

pub fn parse_responses_tool_choice(tc: &Value) -> InternalToolChoice {
    match tc {
        Value::String(s) => InternalToolChoice::Simple(s.clone()),
        Value::Object(_) => {
            if tc.get("type").and_then(|t| t.as_str()) == Some("function") {
                if let Some(name) = tc.get("name").and_then(|n| n.as_str()) {
                    return InternalToolChoice::Named {
                        name: name.to_string(),
                        namespace: None,
                    };
                }
            }
            InternalToolChoice::Simple("auto".into())
        }
        _ => InternalToolChoice::Simple("auto".into()),
    }
}

// ===================== emit：IR -> 协议 JSON =====================

/// 把 IR 渲染为 Responses 协议请求体。
pub fn emit_responses_req(ir: &InternalRequest) -> Value {
    let mut input = Vec::<Value>::new();

    for m in &ir.messages {
        match m.role {
            InternalRole::System => {}
            InternalRole::User => {
                let blocks = super::ir_content_to_responses_blocks(&m.content, "user");
                if !blocks.is_empty() {
                    input.push(json!({"type":"message","role":"user","content":blocks}));
                }
            }
            InternalRole::Assistant => {
                if let Some(r) = &m.reasoning {
                    if !r.thinking.is_empty() {
                        let mut item = json!({
                            "type":"reasoning",
                            "summary":[{"type":"summary_text","text":r.thinking}]
                        });
                        if let Some(sig) = &r.signature {
                            if !sig.is_empty() {
                                item["encrypted_content"] = json!(sig);
                            }
                        }
                        input.push(item);
                    }
                }
                let blocks = super::ir_content_to_responses_blocks(&m.content, "assistant");
                if !blocks.is_empty() {
                    input.push(json!({"type":"message","role":"assistant","content":blocks}));
                }
                if !m.tool_calls.is_empty() {
                    for tc in &m.tool_calls {
                        input.push(json!({
                            "type":"function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments
                        }));
                    }
                }
            }
            InternalRole::Tool => {
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                let output = super::ir_content_to_text(&m.content);
                input.push(json!({
                    "type":"function_call_output",
                    "call_id": call_id,
                    "output": output
                }));
            }
        }
    }

    let tools: Vec<Value> = if ir.tools.is_empty() {
        Vec::new()
    } else {
        ir.tools
            .iter()
            .map(|t| {
                json!({
                    "type":"function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect()
    };

    let mut out = json!({
        "model": ir.model,
        "input": input,
        "stream": ir.stream,
    });
    if let Some(sys) = &ir.system {
        if !sys.is_empty() {
            out["instructions"] = json!(sys);
        }
    }
    if !tools.is_empty() {
        out["tools"] = json!(tools);
    }
    if let Some(tc) = &ir.tool_choice {
        out["tool_choice"] = emit_responses_tool_choice(tc);
    }
    if let Some(t) = ir.temperature {
        out["temperature"] = json!(t);
    }
    if let Some(t) = ir.top_p {
        out["top_p"] = json!(t);
    }
    if let Some(mt) = ir.max_tokens {
        out["max_output_tokens"] = json!(mt);
    }
    if let Some(r) = &ir.reasoning {
        if let Some(effort) = &r.effort {
            if !effort.is_empty() {
                let summary = r.summary.clone().unwrap_or_else(|| "auto".to_string());
                out["reasoning"] = json!({"effort": effort, "summary": summary});
            }
        }
    }
    if let Some(p) = ir.parallel_tool_calls {
        out["parallel_tool_calls"] = json!(p);
    }
    for (k, v) in &ir.extensions {
        out[k] = v.clone();
    }
    strip_cache_control(&mut out);
    out
}

pub fn emit_responses_tool_choice(tc: &InternalToolChoice) -> Value {
    match tc {
        InternalToolChoice::Simple(s) => json!(s),
        InternalToolChoice::Named { name, .. } => json!({"type":"function","name":name}),
    }
}

// ===================== resp：协议 JSON -> IR =====================

pub fn parse_responses_resp(src: &Value) -> InternalResponse {
    let mut resp = InternalResponse {
        id: src.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ..Default::default()
    };
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut signatures: Vec<String> = Vec::new();
    if let Some(output) = src.get("output").and_then(|o| o.as_array()) {
        for item in output {
            let itype = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match itype {
                "message" => {
                    let text = super::responses_content_to_text(item.get("content"), "assistant");
                    if !text.is_empty() {
                        text_parts.push(text);
                    }
                }
                "reasoning" => {
                    let summary_text = item
                        .get("summary")
                        .and_then(|s| s.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|p| {
                                    if p.get("type").and_then(|t| t.as_str()) == Some("summary_text") {
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
                        reasoning_parts.push(summary_text);
                    }
                    if let Some(enc) = item.get("encrypted_content").and_then(|x| x.as_str()) {
                        if !enc.is_empty() {
                            signatures.push(enc.to_string());
                        }
                    }
                }
                "function_call" => {
                    let id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let args = item.get("arguments").and_then(|x| x.as_str()).unwrap_or("{}").to_string();
                    resp.tool_calls.push(InternalToolCall {
                        id,
                        name,
                        arguments: args,
                        namespace: None,
                        custom_input: None,
                    });
                }
                _ => {}
            }
        }
    }
    if !text_parts.is_empty() {
        resp.content.push(InternalContent::Text { text: text_parts.join("") });
    }
    if !reasoning_parts.is_empty() {
        resp.reasoning = Some(InternalReasoningBlock {
            thinking: reasoning_parts.join("\n"),
            signature: if signatures.is_empty() {
                None
            } else {
                Some(signatures.join("\n"))
            },
            redacted: false,
        });
    }
    let status = src.get("status").and_then(|x| x.as_str()).unwrap_or("completed");
    if !resp.tool_calls.is_empty() {
        resp.finish_reason = InternalFinishReason::ToolCalls;
    } else {
        resp.finish_reason = match status {
            "completed" => InternalFinishReason::Stop,
            "incomplete" => InternalFinishReason::Length,
            _ => InternalFinishReason::Stop,
        };
    }
    if let Some(u) = src.get("usage") {
        resp.usage = parse_responses_usage(u);
    }
    resp
}

pub fn parse_responses_usage(u: &Value) -> InternalUsage {
    let it = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let cc = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let cr = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    InternalUsage {
        input_tokens: it + cc + cr,
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: cr,
        cache_creation_tokens: cc,
        reasoning_tokens: 0,
    }
}

// ===================== emit：IR -> 协议 JSON（resp） =====================

pub fn emit_responses_resp(ir: &InternalResponse) -> Value {
    let mut output = Vec::new();
    if let Some(r) = &ir.reasoning {
        if !r.thinking.is_empty() {
            let mut item = json!({
                "type":"reasoning",
                "summary":[{"type":"summary_text","text":r.thinking}]
            });
            if let Some(sig) = &r.signature {
                if !sig.is_empty() {
                    item["encrypted_content"] = json!(sig);
                }
            }
            output.push(item);
        }
    }
    let text = super::ir_content_to_text(&ir.content);
    if !text.is_empty() {
        output.push(json!({
            "type":"message","role":"assistant",
            "content":[{"type":"output_text","text":text}]
        }));
    }
    for tc in &ir.tool_calls {
        output.push(json!({
            "type":"function_call","call_id":tc.id,"name":tc.name,"arguments":tc.arguments
        }));
    }

    let mut usage = json!({});
    let it = ir.usage.input_tokens;
    let ot = ir.usage.output_tokens;
    usage["input_tokens"] = json!(it);
    usage["output_tokens"] = json!(ot);
    usage["total_tokens"] = json!(it + ot);

    let id = if ir.id.is_empty() {
        format!("resp_{}", rand_id())
    } else {
        ir.id.clone()
    };

    json!({
        "id": id,
        "object": "response",
        "model": ir.model,
        "output": output,
        "status": "completed",
        "usage": usage
    })
}
