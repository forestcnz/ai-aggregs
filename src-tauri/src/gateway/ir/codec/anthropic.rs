use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::error::AppError;
use crate::gateway::convert_helpers::{clean_schema, strip_cache_control, strip_leading_anthropic_billing_header};
use crate::gateway::converter::rand_id;
use crate::gateway::ir::{
    InternalContent, InternalFinishReason, InternalMessage, InternalReasoning,
    InternalReasoningBlock, InternalRequest, InternalResponse, InternalRole, InternalTool,
    InternalToolCall, InternalToolChoice, InternalToolKind, InternalUsage,
};
use crate::gateway::reasoning_bridge;

// ===================== parse：协议 JSON -> IR =====================

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
                            let content = super::block_content_to_text(b.get("content"));
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

pub fn parse_anthropic_tool_choice(tc: &Value) -> InternalToolChoice {
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

// ===================== emit：IR -> 协议 JSON =====================

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
                let blocks = super::ir_content_to_anthropic_blocks(&m.content);
                if !blocks.is_empty() {
                    super::push_anthropic_user(&mut messages, json!({"role":"user","content":blocks}));
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
                let text = super::ir_content_to_text(&m.content);
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
                let content = super::ir_content_to_text(&m.content);
                super::push_anthropic_user(
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

pub fn emit_anthropic_tool_choice(tc: &InternalToolChoice) -> Value {
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

// ===================== resp：协议 JSON -> IR =====================

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

pub fn parse_anthropic_finish_reason(sr: &str) -> InternalFinishReason {
    match sr {
        "end_turn" | "stop_sequence" | "pause_turn" => InternalFinishReason::Stop,
        "tool_use" => InternalFinishReason::ToolCalls,
        "max_tokens" | "model_context_window_exceeded" => InternalFinishReason::Length,
        "refusal" | "unsafe_content" => InternalFinishReason::ContentFilter,
        _ => InternalFinishReason::Stop,
    }
}

pub fn parse_anthropic_usage(u: &Value) -> InternalUsage {
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

// ===================== emit：IR -> 协议 JSON（resp） =====================

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
    let text = super::ir_content_to_text(&ir.content);
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

pub fn emit_anthropic_stop_reason(fr: InternalFinishReason) -> &'static str {
    match fr {
        InternalFinishReason::Stop => "end_turn",
        InternalFinishReason::Length => "max_tokens",
        InternalFinishReason::ToolCalls => "tool_use",
        InternalFinishReason::ContentFilter => "refusal",
    }
}
