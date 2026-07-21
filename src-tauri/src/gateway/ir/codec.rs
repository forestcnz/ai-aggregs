//! 非流式 req/resp 与 IR 的双向映射。
//!
//! 6 个 parse 函数：协议 JSON -> `InternalRequest`/`InternalResponse`
//! 6 个 emit 函数：`InternalRequest`/`InternalResponse` -> 协议 JSON
//!
//! 协议特有处理在 parse 阶段做"中立化"（如剥离 billing header、编码 envelope），
//! 在 emit 阶段做"目标化"（如解码 envelope、规范化 schema、根据模型自适应 thinking）。
//!
//! 这层完成后，converter/dispatcher 只需 A->IR->B 单跳，消除原 Responses<->Anthropic 双跳。

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::gateway::convert_helpers::{
    clean_schema, strip_cache_control, strip_leading_anthropic_billing_header,
};
use crate::gateway::converter::{created_now, rand_id};
use crate::gateway::ir::{
    InternalContent, InternalFinishReason, InternalMessage, InternalReasoning,
    InternalReasoningBlock, InternalRequest, InternalResponse, InternalRole, InternalTool,
    InternalToolCall, InternalToolChoice, InternalToolKind, InternalUsage,
};
use crate::gateway::reasoning_bridge;
use crate::gateway::tools as codex_tools;
use crate::infra::error::AppError;

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
                    let text = chat_content_to_text(m.get("content"));
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
                    let content = chat_content_to_blocks(m.get("content"));
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

fn parse_chat_tool_call(tc: &Value) -> Option<InternalToolCall> {
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

fn parse_chat_tool_choice(tc: &Value) -> InternalToolChoice {
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

/// 把 Anthropic 协议请求体解析为 IR。
///
/// parse 阶段做的事：
/// - system 字段（字符串或数组）合并到 IR.system（剥离 billing header）
/// - messages 数组中 assistant 的 thinking 块用 reasoning_bridge 编码为 envelope signature
///   （避免跨协议经 Chat 时丢失原 Anthropic signature）
/// - tool_use / tool_result 映射到 IR
/// - tools 数组的 input_schema 透传（规范化留给 emit 阶段）
/// - thinking 配置映射到 IR.reasoning.budget_tokens
pub fn parse_anthropic_req(src: &Value) -> Result<InternalRequest, AppError> {
    let mut messages = Vec::new();

    let mut system_text = String::new();
    if let Some(sys) = src.get("system") {
        let text = match sys {
            Value::String(s) => s.clone(),
            Value::Array(arr) => arr
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        let text = strip_leading_anthropic_billing_header(&text);
        system_text = text.to_string();
    }

    if let Some(arr) = src.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
            let ir_role = match role {
                "user" => InternalRole::User,
                "assistant" => InternalRole::Assistant,
                _ => InternalRole::User,
            };

            // user message 中可能含 tool_result，需要拆分为多条 IR 消息
            if role == "user" {
                if let Some(blocks) = m.get("content").and_then(|c| c.as_array()) {
                    let mut text_parts: Vec<String> = Vec::new();
                    let mut tool_results: Vec<(String, String)> = Vec::new();
                    for b in blocks {
                        let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        if btype == "tool_result" {
                            let tool_use_id = b.get("tool_use_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let content = block_content_to_text(b.get("content"));
                            tool_results.push((tool_use_id, content));
                        } else if btype == "text" {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                    }
                    if !text_parts.is_empty() {
                        messages.push(InternalMessage {
                            role: InternalRole::User,
                            content: text_parts
                                .into_iter()
                                .map(|t| InternalContent::Text { text: t })
                                .collect(),
                            tool_calls: vec![],
                            tool_call_id: None,
                            reasoning: None,
                        });
                    }
                    for (id, content) in tool_results {
                        messages.push(InternalMessage {
                            role: InternalRole::Tool,
                            content: vec![InternalContent::Text { text: content }],
                            tool_calls: vec![],
                            tool_call_id: Some(id),
                            reasoning: None,
                        });
                    }
                    continue;
                }
            }

            let mut msg = InternalMessage {
                role: ir_role,
                content: vec![],
                tool_calls: vec![],
                tool_call_id: None,
                reasoning: None,
            };

            if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                if !s.is_empty() {
                    msg.content.push(InternalContent::Text { text: s.to_string() });
                }
            } else if let Some(blocks) = m.get("content").and_then(|c| c.as_array()) {
                let mut text_parts = Vec::new();
                let mut reasoning_parts = Vec::new();
                let mut signatures: Vec<String> = Vec::new();
                let mut tool_calls = Vec::new();
                for b in blocks {
                    let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match btype {
                        "text" => {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        "thinking" => {
                            if let Some(t) = b.get("thinking").and_then(|x| x.as_str()) {
                                if !t.is_empty() {
                                    reasoning_parts.push(t.to_string());
                                }
                            }
                            if let Some(s) = b.get("signature").and_then(|x| x.as_str()) {
                                if !s.is_empty() {
                                    if let Some(envelope) = reasoning_bridge::encode_anthropic_thinking(b) {
                                        signatures.push(envelope);
                                    } else {
                                        signatures.push(s.to_string());
                                    }
                                }
                            }
                        }
                        "redacted_thinking" => {
                            reasoning_parts.push("[redacted_thinking]".to_string());
                        }
                        "server_tool_use" => {
                            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("server_tool");
                            text_parts.push(format!("[server_tool_use: {name}]"));
                        }
                        "web_search_tool_result"
                        | "web_fetch_tool_result"
                        | "code_execution_tool_result"
                        | "bash_code_execution_tool_result"
                        | "text_editor_code_execution_tool_result" => {
                            text_parts.push(format!("[{btype}]"));
                        }
                        "fallback" => {
                            if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        "tool_use" => {
                            let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                            let input = b.get("input").cloned().unwrap_or(json!({}));
                            let args = serde_json::to_string(&input).unwrap_or_default();
                            tool_calls.push(InternalToolCall {
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
                if !text_parts.is_empty() {
                    msg.content.extend(text_parts.into_iter().map(|t| InternalContent::Text { text: t }));
                }
                if !reasoning_parts.is_empty() {
                    msg.reasoning = Some(InternalReasoningBlock {
                        thinking: reasoning_parts.join("\n"),
                        signature: if signatures.is_empty() {
                            None
                        } else {
                            Some(signatures.join("\n"))
                        },
                        redacted: false,
                    });
                }
                if !tool_calls.is_empty() {
                    msg.tool_calls = tool_calls;
                }
            }

            let is_empty = msg.content.is_empty()
                && msg.tool_calls.is_empty()
                && msg.reasoning.is_none()
                && msg.tool_call_id.is_none();
            if !is_empty {
                messages.push(msg);
            }
        }
    }

    let mut tools = Vec::new();
    if let Some(arr) = src.get("tools").and_then(|t| t.as_array()) {
        for t in arr {
            tools.push(InternalTool {
                name: t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                description: t.get("description").and_then(|x| x.as_str()).map(String::from),
                parameters: t.get("input_schema").cloned().unwrap_or(json!({})),
                strict: false,
                kind: InternalToolKind::Function,
            });
        }
    }

    let tool_choice = src.get("tool_choice").map(parse_anthropic_tool_choice);

    let mut reasoning = None;
    if let Some(thinking) = src.get("thinking") {
        let t_type = thinking.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if t_type == "enabled" || t_type == "adaptive" {
            let mut r = InternalReasoning::default();
            r.budget_tokens = thinking.get("budget_tokens").and_then(|b| b.as_u64()).map(|v| v as u32);
            r.effort = Some(if t_type == "adaptive" {
                "high".to_string()
            } else {
                let budget = r.budget_tokens.unwrap_or(8192);
                if budget < 4000 {
                    "low".into()
                } else if budget < 16000 {
                    "medium".into()
                } else {
                    "high".into()
                }
            });
            reasoning = Some(r);
        }
    }
    if reasoning.is_none() {
        if let Some(effort) = src.pointer("/output_config/effort").and_then(|v| v.as_str()) {
            let mapped = if effort == "max" { "xhigh" } else { effort };
            reasoning = Some(InternalReasoning {
                effort: Some(mapped.to_string()),
                ..InternalReasoning::default()
            });
        }
    }

    Ok(InternalRequest {
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        system: if system_text.is_empty() { None } else { Some(system_text) },
        messages,
        tools,
        tool_choice,
        max_tokens: src.get("max_tokens").and_then(|x| x.as_u64()).map(|v| v as u32),
        temperature: src.get("temperature").and_then(|x| x.as_f64()).map(|v| v as f32),
        top_p: src.get("top_p").and_then(|x| x.as_f64()).map(|v| v as f32),
        stop: src
            .get("stop_sequences")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        stream: src.get("stream").and_then(|x| x.as_bool()).unwrap_or(false),
        reasoning,
        parallel_tool_calls: None,
        extensions: Map::new(),
        envelopes: HashMap::new(),
    })
}

fn parse_anthropic_tool_choice(tc: &Value) -> InternalToolChoice {
    match tc {
        Value::String(s) => InternalToolChoice::Simple(s.clone()),
        Value::Object(_) => {
            let t = tc.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match t {
                "auto" => InternalToolChoice::Simple("auto".into()),
                "any" => InternalToolChoice::Simple("required".into()),
                "none" => InternalToolChoice::Simple("none".into()),
                "tool" => {
                    let name = tc.get("name").cloned().unwrap_or(json!(""));
                    InternalToolChoice::Named {
                        name: name.as_str().unwrap_or("").to_string(),
                        namespace: None,
                    }
                }
                _ => InternalToolChoice::Simple("auto".into()),
            }
        }
        _ => InternalToolChoice::Simple("auto".into()),
    }
}

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
                        let text = responses_content_to_text(item.get("content"), role);
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
        envelopes: HashMap::new(),
    })
}

fn parse_responses_tool_choice(tc: &Value) -> InternalToolChoice {
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
                let text = ir_content_to_text(&m.content);
                if !text.is_empty() {
                    messages.push(json!({"role":"system","content":text}));
                }
            }
            InternalRole::User => {
                let text = ir_content_to_text(&m.content);
                if !text.is_empty() {
                    messages.push(json!({"role":"user","content":text}));
                }
            }
            InternalRole::Assistant => {
                let text = ir_content_to_text(&m.content);
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
                let text = ir_content_to_text(&m.content);
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

fn emit_chat_tool_choice(tc: &InternalToolChoice) -> Value {
    match tc {
        InternalToolChoice::Simple(s) => json!(s),
        InternalToolChoice::Named { name, .. } => {
            json!({"type":"function","function":{"name":name}})
        }
    }
}

/// 把 IR 渲染为 Anthropic 协议请求体。
///
/// emit 阶段做的事：
/// - IR.system -> system 字段
/// - IR.messages 合并连续 user（Anthropic 严格要求 user/assistant 交替）
/// - reasoning.signature 通过 reasoning_bridge 解码还原原 Anthropic signature
/// - tools.parameters 经 clean_schema 规范化为 input_schema
/// - reasoning.effort -> thinking 配置（根据模型自适应 enabled/adaptive + budget_tokens）
/// - 跨协议方向剥离 cache_control
pub fn emit_anthropic_req(ir: &InternalRequest) -> Value {
    let mut messages = Vec::<Value>::new();

    for m in &ir.messages {
        match m.role {
            InternalRole::System => {}
            InternalRole::User => {
                let blocks = ir_content_to_anthropic_blocks(&m.content);
                if !blocks.is_empty() {
                    push_anthropic_user(&mut messages, json!({"role":"user","content":blocks}));
                }
            }
            InternalRole::Assistant => {
                let mut blocks = Vec::new();
                if let Some(r) = &m.reasoning {
                    if !r.thinking.is_empty() {
                        let mut block = json!({"type":"thinking","thinking":r.thinking});
                        if let Some(sig) = &r.signature {
                            if !sig.is_empty() {
                                if let Some(decoded) = reasoning_bridge::decode_envelope(sig) {
                                    if let Some(orig_sig) = decoded.get("signature").and_then(|s| s.as_str()) {
                                        block["signature"] = json!(orig_sig);
                                    } else {
                                        block["signature"] = json!(sig);
                                    }
                                } else {
                                    block["signature"] = json!(sig);
                                }
                            }
                        }
                        blocks.push(block);
                    }
                }
                let text = ir_content_to_text(&m.content);
                if !text.is_empty() {
                    blocks.push(json!({"type":"text","text":text}));
                }
                if !m.tool_calls.is_empty() {
                    for tc in &m.tool_calls {
                        let input: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                        blocks.push(json!({
                            "type":"tool_use",
                            "id": tc.id,
                            "name": tc.name,
                            "input": input
                        }));
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role":"assistant","content":blocks}));
                }
            }
            InternalRole::Tool => {
                let tool_use_id = m.tool_call_id.clone().unwrap_or_default();
                let content = ir_content_to_text(&m.content);
                push_anthropic_user(
                    &mut messages,
                    json!({"role":"user","content":[{
                        "type":"tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content
                    }]}),
                );
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
                    "name": t.name,
                    "description": t.description,
                    "input_schema": clean_schema(t.parameters.clone()),
                })
            })
            .collect()
    };

    let mut out = json!({
        "model": ir.model,
        "messages": messages,
        "max_tokens": ir.max_tokens.unwrap_or(4096),
        "stream": ir.stream,
    });
    if let Some(sys) = &ir.system {
        if !sys.is_empty() {
            out["system"] = json!(sys);
        }
    }
    if !tools.is_empty() {
        out["tools"] = json!(tools);
    }
    if let Some(tc) = &ir.tool_choice {
        out["tool_choice"] = emit_anthropic_tool_choice(tc);
    }
    if let Some(t) = ir.temperature {
        out["temperature"] = json!(t);
    }
    if let Some(t) = ir.top_p {
        out["top_p"] = json!(t);
    }
    if !ir.stop.is_empty() {
        out["stop_sequences"] = json!(ir.stop);
    }
    if let Some(r) = &ir.reasoning {
        if let Some(effort) = &r.effort {
            if !effort.is_empty() && out.get("thinking").is_none() {
                let model = &ir.model;
                let needs_adaptive = model.contains("4-6")
                    || model.contains("4-7")
                    || model.contains("4-8")
                    || model.contains("sonnet-5")
                    || model.contains("fable")
                    || model.contains("mythos");
                let budget: u32 = match effort.to_ascii_lowercase().as_str() {
                    "low" | "minimal" => 2048,
                    "medium" => 8192,
                    "high" => 16384,
                    "xhigh" | "max" => 24576,
                    _ => 8192,
                };
                out["thinking"] = if needs_adaptive {
                    json!({"type": "adaptive"})
                } else {
                    json!({"type": "enabled", "budget_tokens": budget})
                };
            }
        }
    }
    strip_cache_control(&mut out);
    out
}

fn emit_anthropic_tool_choice(tc: &InternalToolChoice) -> Value {
    match tc {
        InternalToolChoice::Simple(s) => match s.as_str() {
            "auto" => json!({"type":"auto"}),
            "none" => json!({"type":"none"}),
            "required" => json!({"type":"any"}),
            _ => json!({"type":"auto"}),
        },
        InternalToolChoice::Named { name, .. } => json!({"type":"tool","name":name}),
    }
}

/// 把 IR 渲染为 Responses 协议请求体。
pub fn emit_responses_req(ir: &InternalRequest) -> Value {
    let mut input = Vec::<Value>::new();

    for m in &ir.messages {
        match m.role {
            InternalRole::System => {}
            InternalRole::User => {
                let blocks = ir_content_to_responses_blocks(&m.content, "user");
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
                let blocks = ir_content_to_responses_blocks(&m.content, "assistant");
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
                let output = ir_content_to_text(&m.content);
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

fn emit_responses_tool_choice(tc: &InternalToolChoice) -> Value {
    match tc {
        InternalToolChoice::Simple(s) => json!(s),
        InternalToolChoice::Named { name, .. } => json!({"type":"function","name":name}),
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

fn parse_chat_finish_reason(fr: &str) -> InternalFinishReason {
    match fr {
        "stop" => InternalFinishReason::Stop,
        "length" => InternalFinishReason::Length,
        "tool_calls" | "function_call" => InternalFinishReason::ToolCalls,
        "content_filter" => InternalFinishReason::ContentFilter,
        _ => InternalFinishReason::Stop,
    }
}

fn parse_chat_usage(u: &Value) -> InternalUsage {
    InternalUsage {
        input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_creation_tokens: u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        reasoning_tokens: 0,
    }
}

pub fn parse_anthropic_resp(src: &Value) -> InternalResponse {
    let mut resp = InternalResponse {
        id: src.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        model: src.get("model").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        ..Default::default()
    };
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut signatures: Vec<String> = Vec::new();
    if let Some(blocks) = src.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match btype {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                "thinking" => {
                    if let Some(t) = b.get("thinking").and_then(|x| x.as_str()) {
                        if !t.is_empty() {
                            reasoning_parts.push(t.to_string());
                        }
                    }
                    if let Some(s) = b.get("signature").and_then(|x| x.as_str()) {
                        if !s.is_empty() {
                            signatures.push(s.to_string());
                        }
                    }
                }
                "redacted_thinking" => {
                    reasoning_parts.push("[redacted_thinking]".to_string());
                }
                "server_tool_use" => {
                    let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("server_tool");
                    text_parts.push(format!("[server_tool_use: {name}]"));
                }
                "web_search_tool_result"
                | "web_fetch_tool_result"
                | "code_execution_tool_result"
                | "bash_code_execution_tool_result"
                | "text_editor_code_execution_tool_result" => {
                    text_parts.push(format!("[{btype}]"));
                }
                "fallback" => {
                    if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                "tool_use" => {
                    let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let input = b.get("input").cloned().unwrap_or(json!({}));
                    let args = serde_json::to_string(&input).unwrap_or_default();
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
    if let Some(sr) = src.get("stop_reason").and_then(|x| x.as_str()) {
        resp.finish_reason = parse_anthropic_finish_reason(sr);
    }
    if let Some(u) = src.get("usage") {
        resp.usage = parse_anthropic_usage(u);
    }
    resp
}

fn parse_anthropic_finish_reason(sr: &str) -> InternalFinishReason {
    match sr {
        "end_turn" | "stop_sequence" | "pause_turn" => InternalFinishReason::Stop,
        "tool_use" => InternalFinishReason::ToolCalls,
        "max_tokens" | "model_context_window_exceeded" => InternalFinishReason::Length,
        "refusal" | "unsafe_content" => InternalFinishReason::ContentFilter,
        _ => InternalFinishReason::Stop,
    }
}

fn parse_anthropic_usage(u: &Value) -> InternalUsage {
    let input_tokens = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let cache_creation = u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    let cache_read = u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
    InternalUsage {
        input_tokens: input_tokens + cache_creation + cache_read,
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_read_tokens: cache_read,
        cache_creation_tokens: cache_creation,
        reasoning_tokens: 0,
    }
}

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
                    let text = responses_content_to_text(item.get("content"), "assistant");
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

fn parse_responses_usage(u: &Value) -> InternalUsage {
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

pub fn emit_chat_resp(ir: &InternalResponse) -> Value {
    let mut message = json!({"role":"assistant"});
    let text = ir_content_to_text(&ir.content);
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

fn emit_chat_finish_reason(fr: InternalFinishReason) -> &'static str {
    match fr {
        InternalFinishReason::Stop => "stop",
        InternalFinishReason::Length => "length",
        InternalFinishReason::ToolCalls => "tool_calls",
        InternalFinishReason::ContentFilter => "content_filter",
    }
}

pub fn emit_anthropic_resp(ir: &InternalResponse) -> Value {
    let mut content = Vec::new();
    if let Some(r) = &ir.reasoning {
        if !r.thinking.is_empty() {
            let mut block = json!({"type":"thinking","thinking":r.thinking});
            if let Some(sig) = &r.signature {
                if !sig.is_empty() {
                    block["signature"] = json!(sig);
                }
            }
            content.push(block);
        }
    }
    let text = ir_content_to_text(&ir.content);
    if !text.is_empty() {
        content.push(json!({"type":"text","text":text}));
    }
    for tc in &ir.tool_calls {
        let input: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
        content.push(json!({"type":"tool_use","id":tc.id,"name":tc.name,"input":input}));
    }

    let stop_reason = emit_anthropic_stop_reason(ir.finish_reason);

    let mut usage = json!({});
    if ir.usage.input_tokens > 0 {
        usage["input_tokens"] = json!(ir.usage.input_tokens);
    }
    if ir.usage.output_tokens > 0 {
        usage["output_tokens"] = json!(ir.usage.output_tokens);
    }

    let id = if ir.id.is_empty() {
        format!("msg_{}", rand_id())
    } else {
        ir.id.clone()
    };

    json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": ir.model,
        "content": content,
        "stop_reason": stop_reason,
        "usage": usage
    })
}

fn emit_anthropic_stop_reason(fr: InternalFinishReason) -> &'static str {
    match fr {
        InternalFinishReason::Stop => "end_turn",
        InternalFinishReason::Length => "max_tokens",
        InternalFinishReason::ToolCalls => "tool_use",
        InternalFinishReason::ContentFilter => "refusal",
    }
}

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
    let text = ir_content_to_text(&ir.content);
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

// ===================== 共享 helper =====================

fn chat_content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                let t = b.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                if t == "text" {
                    b.get("text").and_then(|x| x.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn chat_content_to_blocks(content: Option<&Value>) -> Vec<InternalContent> {
    let mut out = Vec::new();
    match content {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                out.push(InternalContent::Text { text: s.clone() });
            }
        }
        Some(Value::Array(arr)) => {
            for b in arr {
                let t = b.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                if t == "text" {
                    if let Some(txt) = b.get("text").and_then(|x| x.as_str()) {
                        if !txt.is_empty() {
                            out.push(InternalContent::Text { text: txt.to_string() });
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

fn block_content_to_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn responses_content_to_text(content: Option<&Value>, role: &str) -> String {
    let want = if role == "assistant" { "output_text" } else { "input_text" };
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                let t = b.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if t == want || t == "text" {
                    b.get("text").and_then(|x| x.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn ir_content_to_text(content: &[InternalContent]) -> String {
    let mut s = String::new();
    for c in content {
        if let InternalContent::Text { text } = c {
            s.push_str(text);
        }
    }
    s
}

fn ir_content_to_anthropic_blocks(content: &[InternalContent]) -> Vec<Value> {
    let mut blocks = Vec::new();
    for c in content {
        if let InternalContent::Text { text } = c {
            if !text.is_empty() {
                blocks.push(json!({"type":"text","text":text}));
            }
        }
    }
    blocks
}

fn ir_content_to_responses_blocks(content: &[InternalContent], role: &str) -> Vec<Value> {
    let text_type = if role == "assistant" { "output_text" } else { "input_text" };
    let mut blocks = Vec::new();
    for c in content {
        if let InternalContent::Text { text } = c {
            if !text.is_empty() {
                blocks.push(json!({"type":text_type,"text":text}));
            }
        }
    }
    blocks
}

/// 把连续多条 user 消息合并为一条（content 数组拼接）。
/// Anthropic 上游严格要求 user/assistant 交替，连续 user 会被拒。
fn push_anthropic_user(msgs: &mut Vec<Value>, m: Value) {
    if let Some(last) = msgs.last_mut() {
        if last.get("role").and_then(|r| r.as_str()) == Some("user") {
            if let (Some(a), Some(b)) = (
                last.get_mut("content").and_then(|c| c.as_array_mut()),
                m.get("content").and_then(|c| c.as_array()),
            ) {
                a.extend(b.iter().cloned());
                return;
            }
        }
    }
    msgs.push(m);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_chat_basic_request() {
        let src = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hi"}
            ]
        });
        let ir = parse_chat_req(&src).unwrap();
        assert_eq!(ir.model, "gpt-5");
        assert_eq!(ir.system.as_deref(), Some("You are helpful"));
        assert_eq!(ir.messages.len(), 1);
        assert!(matches!(ir.messages[0].role, InternalRole::User));
    }

    #[test]
    fn round_trip_chat_anthropic_chat_preserves_tool_call() {
        let original = json!({
            "model": "gpt-4",
            "max_tokens": 1024,
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "weather", "arguments": "{\"city\":\"SF\"}"}
                }]
            }]
        });
        let ir = parse_chat_req(&original).unwrap();
        let anthropic = emit_anthropic_req(&ir);
        let ir2 = parse_anthropic_req(&anthropic).unwrap();
        let chat_back = emit_chat_req(&ir2);
        let back_messages = chat_back["messages"].as_array().unwrap();
        let back_assistant = back_messages
            .iter()
            .find(|m| m["role"].as_str() == Some("assistant"))
            .expect("should have assistant message");
        let tool_calls = back_assistant["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls[0]["function"]["name"], "weather");
    }
}
