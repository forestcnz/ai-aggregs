use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::error::AppError;
use crate::gateway::convert_helpers::{clean_schema, strip_leading_anthropic_billing_header};
use crate::gateway::converter::{created_now, rand_id};
use crate::gateway::ir::{
    InternalContent, InternalFinishReason, InternalMessage, InternalReasoning,
    InternalReasoningBlock, InternalRequest, InternalResponse, InternalRole, InternalTool,
    InternalToolCall, InternalToolChoice, InternalToolKind, InternalUsage,
};
use crate::gateway::tools as codex_tools;

// ===================== parse：协议 JSON -> IR =====================

/// 把 Chat 协议请求体解析为 IR。
///
/// parse 阶段做的事：
/// - system message 合并到 IR.system（剥离 Anthropic billing header）
/// - tool_calls / tool_call_id 等映射到 IR.messages
/// - reasoning_content + reasoning_signature 映射到 IR.messages[i].reasoning
///   （signature 直接保留原值；若为 envelope 字符串，emit_anthropic_req 时再解码）
/// - tools 数组中立化为 IR.tools（function/参数原样保留，规范化留给 emit 阶段）
pub fn parse_chat_req(src: &Value) -> Result<InternalRequest, AppError> {
    let mut messages = Vec::new();
    let mut system_parts: Vec<String> = Vec::new();

    if let Some(arr) = src.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            match role {
                "system" => {
                    let text = super::chat_content_to_text(m.get("content"));
                    let text = strip_leading_anthropic_billing_header(&text);
                    if !text.is_empty() {
                        system_parts.push(text.to_string());
                    }
                }
                "user" | "assistant" | "tool" => {
                    let ir_role = match role {
                        "user" => InternalRole::User,
                        "assistant" => InternalRole::Assistant,
                        "tool" => InternalRole::Tool,
                        _ => unreachable!(),
                    };
                    let content = super::chat_content_to_blocks(m.get("content"));
                    let mut msg = InternalMessage {
                        role: ir_role,
                        content,
                        tool_calls: vec![],
                        tool_call_id: None,
                        reasoning: None,
                    };
                    if role == "assistant" {
                        if let Some(rc) = m.get("reasoning_content").and_then(|x| x.as_str()) {
                            if !rc.is_empty() {
                                let signature = m
                                    .get("reasoning_signature")
                                    .and_then(|x| x.as_str())
                                    .map(String::from);
                                msg.reasoning = Some(InternalReasoningBlock {
                                    thinking: rc.to_string(),
                                    signature,
                                    redacted: false,
                                });
                            }
                        }
                        if let Some(tcs) = m.get("tool_calls").and_then(|x| x.as_array()) {
                            for tc in tcs {
                                if let Some(tc_parsed) = parse_chat_tool_call(tc) {
                                    msg.tool_calls.push(tc_parsed);
                                }
                            }
                        }
                    }
                    if role == "tool" {
                        msg.tool_call_id = m
                            .get("tool_call_id")
                            .and_then(|x| x.as_str())
                            .map(String::from);
                    }
                    messages.push(msg);
                }
                _ => {}
            }
        }
    }

    let mut tools = Vec::new();
    if let Some(arr) = src.get("tools").and_then(|t| t.as_array()) {
        for t in arr {
            let tool_type = t.get("type").and_then(|x| x.as_str()).unwrap_or("function");
            match tool_type {
                "function" => {
                    if let Some(f) = t.get("function") {
                        tools.push(InternalTool {
                            name: f.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            description: f.get("description").and_then(|x| x.as_str()).map(String::from),
                            parameters: f.get("parameters").cloned().unwrap_or(json!({})),
                            strict: f.get("strict").and_then(|x| x.as_bool()).unwrap_or(false),
                            kind: InternalToolKind::Function,
                        });
                    }
                }
                "namespace" => {
                    let ns_name = t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let children_raw = t
                        .get("tools")
                        .and_then(|x| x.as_array())
                        .or_else(|| t.get("children").and_then(|c| c.as_array()));
                    if let Some(children) = children_raw {
                        for (flat, _owner) in codex_tools::flatten_namespace_children(&ns_name, children) {
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
                    if !tools.iter().any(|t| t.name == codex_tools::TOOL_SEARCH_PROXY_NAME) {
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
                    }
                }
                _ => {}
            }
        }
    }

    let tool_choice = src.get("tool_choice").map(parse_chat_tool_choice);

    let mut reasoning = None;
    if let Some(effort) = src.get("reasoning_effort").and_then(|v| v.as_str()) {
        if !effort.is_empty() {
            reasoning = Some(InternalReasoning {
                effort: Some(effort.to_string()),
                ..InternalReasoning::default()
            });
        }
    }

    let mut extensions = Map::new();
    if let Some(st) = src.get("service_tier") {
        extensions.insert("service_tier".into(), st.clone());
    }
    if let Some(st) = src.get("store") {
        extensions.insert("store".into(), st.clone());
    }

    Ok(InternalRequest {
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        system: if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        },
        messages,
        tools,
        tool_choice,
        max_tokens: src.get("max_tokens").and_then(|x| x.as_u64()).map(|v| v as u32),
        temperature: src.get("temperature").and_then(|x| x.as_f64()).map(|v| v as f32),
        top_p: src.get("top_p").and_then(|x| x.as_f64()).map(|v| v as f32),
        stop: src
            .get("stop")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        stream: src.get("stream").and_then(|x| x.as_bool()).unwrap_or(false),
        reasoning,
        parallel_tool_calls: src.get("parallel_tool_calls").and_then(|x| x.as_bool()),
        extensions,
        envelopes: HashMap::new(),
    })
}

pub fn parse_chat_tool_call(tc: &Value) -> Option<InternalToolCall> {
    let id = tc.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let func = tc.get("function")?;
    let name = func.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let args = func.get("arguments").and_then(|x| x.as_str()).unwrap_or("{}").to_string();
    Some(InternalToolCall {
        id,
        name,
        arguments: args,
        namespace: None,
        custom_input: None,
    })
}

pub fn parse_chat_tool_choice(tc: &Value) -> InternalToolChoice {
    match tc {
        Value::String(s) => InternalToolChoice::Simple(s.clone()),
        Value::Object(_) => {
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
            {
                InternalToolChoice::Named {
                    name: name.to_string(),
                    namespace: None,
                }
            } else {
                InternalToolChoice::Simple("auto".into())
            }
        }
        _ => InternalToolChoice::Simple("auto".into()),
    }
}

// ===================== emit：IR -> 协议 JSON =====================

/// 把 IR 渲染为 Chat 协议请求体。
pub fn emit_chat_req(ir: &InternalRequest) -> Value {
    let mut messages = Vec::<Value>::new();

    if let Some(sys) = &ir.system {
        if !sys.is_empty() {
            messages.push(json!({"role":"system","content":sys}));
        }
    }

    for m in &ir.messages {
        match m.role {
            InternalRole::System => {
                let text = super::ir_content_to_text(&m.content);
                if !text.is_empty() {
                    messages.push(json!({"role":"system","content":text}));
                }
            }
            InternalRole::User => {
                let text = super::ir_content_to_text(&m.content);
                if !text.is_empty() {
                    messages.push(json!({"role":"user","content":text}));
                }
            }
            InternalRole::Assistant => {
                let text = super::ir_content_to_text(&m.content);
                let mut msg = json!({"role":"assistant"});
                msg["content"] = if text.is_empty() { json!("") } else { json!(text) };
                if let Some(r) = &m.reasoning {
                    if !r.thinking.is_empty() {
                        msg["reasoning_content"] = json!(r.thinking);
                    }
                    if let Some(sig) = &r.signature {
                        if !sig.is_empty() {
                            msg["reasoning_signature"] = json!(sig);
                        }
                    }
                }
                if !m.tool_calls.is_empty() {
                    let tcs: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id, "type":"function",
                                "function":{"name":tc.name,"arguments":tc.arguments}
                            })
                        })
                        .collect();
                    msg["tool_calls"] = json!(tcs);
                }
                if msg.get("content").is_some() || msg.get("tool_calls").is_some() {
                    messages.push(msg);
                }
            }
            InternalRole::Tool => {
                let text = super::ir_content_to_text(&m.content);
                let tool_call_id = m.tool_call_id.clone().unwrap_or_default();
                messages.push(json!({"role":"tool","tool_call_id":tool_call_id,"content":text}));
            }
        }
    }

    let mut out = json!({
        "model": ir.model,
        "messages": messages,
        "stream": ir.stream,
    });
    if let Some(mt) = ir.max_tokens {
        out["max_tokens"] = json!(mt);
    }
    if let Some(t) = ir.temperature {
        out["temperature"] = json!(t);
    }
    if let Some(t) = ir.top_p {
        out["top_p"] = json!(t);
    }
    if !ir.stop.is_empty() {
        out["stop"] = json!(ir.stop);
    }
    if !ir.tools.is_empty() {
        let tools: Vec<Value> = ir
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type":"function",
                    "function":{
                        "name": t.name,
                        "description": t.description,
                        // 规范化 JSON Schema：补 type/properties，删 format:uri（严格上游如 GLM/Qwen 要求）
                        "parameters": clean_schema(t.parameters.clone()),
                    }
                })
            })
            .collect();
        out["tools"] = json!(tools);
    }
    if let Some(tc) = &ir.tool_choice {
        out["tool_choice"] = emit_chat_tool_choice(tc);
    }
    if let Some(r) = &ir.reasoning {
        if let Some(effort) = &r.effort {
            if !effort.is_empty() {
                out["reasoning_effort"] = json!(effort);
            }
        }
    }
    if let Some(p) = ir.parallel_tool_calls {
        out["parallel_tool_calls"] = json!(p);
    }
    for (k, v) in &ir.extensions {
        out[k] = v.clone();
    }
    out
}

pub fn emit_chat_tool_choice(tc: &InternalToolChoice) -> Value {
    match tc {
        InternalToolChoice::Simple(s) => json!(s),
        InternalToolChoice::Named { name, .. } => {
            json!({"type":"function","function":{"name":name}})
        }
    }
}

// ===================== resp：协议 JSON -> IR =====================

pub fn parse_chat_resp(src: &Value) -> InternalResponse {
    let mut resp = InternalResponse {
        id: src.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ..Default::default()
    };
    if let Some(choice) = src.get("choices").and_then(|c| c.as_array()).and_then(|a| a.first()) {
        if let Some(msg) = choice.get("message") {
            if let Some(rc) = msg.get("reasoning_content").and_then(|x| x.as_str()) {
                if !rc.is_empty() {
                    let sig = msg.get("reasoning_signature").and_then(|x| x.as_str()).map(String::from);
                    resp.reasoning = Some(InternalReasoningBlock {
                        thinking: rc.to_string(),
                        signature: sig,
                        redacted: false,
                    });
                }
            }
            if let Some(t) = msg.get("content").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    resp.content.push(InternalContent::Text { text: t.to_string() });
                }
            }
            if let Some(tcs) = msg.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in tcs {
                    if let Some(parsed) = parse_chat_tool_call(tc) {
                        resp.tool_calls.push(parsed);
                    }
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            resp.finish_reason = parse_chat_finish_reason(fr);
        }
    }
    if let Some(u) = src.get("usage") {
        resp.usage = parse_chat_usage(u);
    }
    resp
}

pub fn parse_chat_finish_reason(fr: &str) -> InternalFinishReason {
    match fr {
        "stop" => InternalFinishReason::Stop,
        "length" => InternalFinishReason::Length,
        "tool_calls" | "function_call" => InternalFinishReason::ToolCalls,
        "content_filter" => InternalFinishReason::ContentFilter,
        _ => InternalFinishReason::Stop,
    }
}

pub fn parse_chat_usage(u: &Value) -> InternalUsage {
    InternalUsage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_creation_tokens: u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        reasoning_tokens: 0,
    }
}

// ===================== emit：IR -> 协议 JSON（resp） =====================

pub fn emit_chat_resp(ir: &InternalResponse) -> Value {
    let mut message = json!({"role":"assistant"});
    let text = super::ir_content_to_text(&ir.content);
    message["content"] = if text.is_empty() { json!("") } else { json!(text) };
    if let Some(r) = &ir.reasoning {
        if !r.thinking.is_empty() {
            message["reasoning_content"] = json!(r.thinking);
        }
        if let Some(sig) = &r.signature {
            if !sig.is_empty() {
                message["reasoning_signature"] = json!(sig);
            }
        }
    }
    if !ir.tool_calls.is_empty() {
        let tcs: Vec<Value> = ir
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id, "type":"function",
                    "function":{"name":tc.name,"arguments":tc.arguments}
                })
            })
            .collect();
        message["tool_calls"] = json!(tcs);
    }

    let finish = emit_chat_finish_reason(ir.finish_reason);

    let mut usage = json!({});
    if ir.usage.input_tokens > 0 {
        usage["prompt_tokens"] = json!(ir.usage.input_tokens);
    }
    if ir.usage.output_tokens > 0 {
        usage["completion_tokens"] = json!(ir.usage.output_tokens);
    }
    if ir.usage.input_tokens > 0 || ir.usage.output_tokens > 0 {
        usage["total_tokens"] = json!(ir.usage.input_tokens + ir.usage.output_tokens);
    }
    if ir.usage.cache_creation_tokens > 0 {
        usage["cache_creation_input_tokens"] = json!(ir.usage.cache_creation_tokens);
    }
    if ir.usage.cache_read_tokens > 0 {
        usage["cache_read_input_tokens"] = json!(ir.usage.cache_read_tokens);
    }

    let id = if ir.id.is_empty() {
        format!("chatcmpl-{}", rand_id())
    } else {
        ir.id.clone()
    };

    json!({
        "id": id,
        "object": "chat.completion",
        "created": created_now(),
        "model": ir.model,
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": finish
        }],
        "usage": usage
    })
}

pub fn emit_chat_finish_reason(fr: InternalFinishReason) -> &'static str {
    match fr {
        InternalFinishReason::Stop => "stop",
        InternalFinishReason::Length => "length",
        InternalFinishReason::ToolCalls => "tool_calls",
        InternalFinishReason::ContentFilter => "content_filter",
    }
}
