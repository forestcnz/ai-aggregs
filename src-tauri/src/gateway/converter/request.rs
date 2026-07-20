//! 请求体转换：4 个方向的请求体转换函数。
//!
//! - Chat ↔ Anthropic：`chat_to_anthropic_req` / `anthropic_to_chat_req`
//! - Chat ↔ Responses：`chat_to_responses_req` / `responses_to_chat_req`
//!
//! Responses ↔ Anthropic 走 Chat 中转（见 `super::mod.rs` 的分发函数）。

use serde_json::{json, Value};

use crate::gateway::convert_helpers::{
    clean_schema, strip_cache_control, strip_leading_anthropic_billing_header,
};
use crate::gateway::converter::helpers::*;
use crate::gateway::{reasoning_bridge, tools};
use crate::infra::error::AppError;

// ---------- Chat -> Anthropic ----------

pub fn chat_to_anthropic_req(src: &Value) -> Value {
    let mut system_parts = Vec::new();
    let mut messages = Vec::<Value>::new();
    let messages_in = src["messages"].as_array().cloned().unwrap_or_default();

    for m in messages_in {
        let role = m["role"].as_str().unwrap_or("user");
        match role {
            "system" => {
                let text = chat_content_to_text(&m["content"]);
                // 剥离 Claude Code 注入的 billing header（仅开头第一行）
                let text = strip_leading_anthropic_billing_header(&text);
                if !text.is_empty() {
                    system_parts.push(text.to_string());
                }
            }
            "user" => {
                let blocks = chat_content_to_anthropic_blocks(&m["content"]);
                if !blocks.is_empty() {
                    push_anthropic_user(&mut messages, json!({"role":"user","content":blocks}));
                }
            }
            "assistant" => {
                let mut blocks = Vec::new();
                // reasoning_content + reasoning_signature → thinking 块（多轮 thinking 完整性）
                if let Some(rc) = m.get("reasoning_content").and_then(|x| x.as_str()) {
                    if !rc.is_empty() {
                        let signature = m
                            .get("reasoning_signature")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        let mut block = json!({"type":"thinking","thinking":rc});
                        if !signature.is_empty() {
                            // 检测 envelope：跨协议（Anthropic→Chat→Anthropic）双跳场景，
                            // 还原原 Anthropic thinking signature。
                            // 若 envelope 内是 OpenAI reasoning item（如 Responses→Chat→Anthropic
                            // 双跳），保留 envelope 字符串作为 signature 以保真数据
                            // （Anthropic 上游可能不识别 OpenAI envelope，但至少数据不丢）。
                            // 非 envelope 字符串直接透传（旧版兼容）。
                            if let Some(decoded) = reasoning_bridge::decode_envelope(signature) {
                                if let Some(orig_sig) =
                                    decoded.get("signature").and_then(|s| s.as_str())
                                {
                                    // Anthropic thinking envelope：还原原 signature
                                    block["signature"] = json!(orig_sig);
                                } else {
                                    // OpenAI reasoning envelope：保留 envelope 字符串
                                    block["signature"] = json!(signature);
                                }
                            } else {
                                block["signature"] = json!(signature);
                            }
                        }
                        blocks.push(block);
                    }
                }
                let text = chat_content_to_text(&m["content"]);
                if !text.is_empty() {
                    blocks.push(json!({"type":"text","text":text}));
                }
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        let args_str = tc["function"]["arguments"].as_str().unwrap_or("{}");
                        let input: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                        blocks.push(json!({
                            "type":"tool_use",
                            "id": tc["id"],
                            "name": tc["function"]["name"],
                            "input": input
                        }));
                    }
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role":"assistant","content":blocks}));
                }
            }
            "tool" => {
                let tool_use_id = m["tool_call_id"].as_str().unwrap_or("");
                let content = m["content"].as_str().unwrap_or("");
                push_anthropic_user(
                    &mut messages,
                    json!({"role":"user","content":[{
                        "type":"tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content
                    }]}),
                );
            }
            _ => {}
        }
    }

    let tools = src.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                Some(json!({
                    "name": f["name"],
                    "description": f["description"],
                    // 规范化 JSON Schema：补 type/properties，删 format:uri
                    "input_schema": clean_schema(f["parameters"].clone()),
                }))
            })
            .collect::<Vec<_>>()
    });

    let mut out = json!({
        "model": src["model"],
        "messages": messages,
        "max_tokens": src.get("max_tokens").cloned().unwrap_or(json!(4096)),
        "stream": src.get("stream").cloned().unwrap_or(json!(false)),
    });
    if !system_parts.is_empty() {
        out["system"] = json!(system_parts.join("\n\n"));
    }
    if let Some(t) = tools {
        out["tools"] = Value::Array(t);
    }
    if let Some(tc) = src.get("tool_choice") {
        out["tool_choice"] = map_tool_choice_chat_to_anthropic(tc);
    }
    if let Some(t) = src.get("temperature") {
        out["temperature"] = t.clone();
    }
    if let Some(t) = src.get("top_p") {
        out["top_p"] = t.clone();
    }
    // reasoning_effort → thinking 参数（Chat 客户端 → Anthropic 上游）
    // Chat 协议的 reasoning_effort 需要转为 Anthropic 的 thinking 配置，
    // 否则上游不知道要启用 extended thinking，thinking 内容不会返回。
    if let Some(effort) = src.get("reasoning_effort").and_then(|v| v.as_str()) {
        if out.get("thinking").is_none() {
            let model = src.get("model").and_then(|m| m.as_str()).unwrap_or("");
            // 新模型用 adaptive，旧模型用 enabled + budget_tokens
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
    // 跨协议方向：剥离 cache_control（避免 GLM/Qwen 等严格上游 400）
    strip_cache_control(&mut out);
    out
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

// ---------- Anthropic -> Chat ----------

pub fn anthropic_to_chat_req(src: &Value) -> Value {
    let mut messages = Vec::<Value>::new();

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
        // 剥离 Claude Code 注入的 billing header（仅开头第一行）
        let text = strip_leading_anthropic_billing_header(&text);
        if !text.is_empty() {
            messages.push(json!({"role":"system","content":text}));
        }
    }

    if let Some(arr) = src.get("messages").and_then(|m| m.as_array()) {
        for m in arr {
            let role = m["role"].as_str().unwrap_or("user");
            let blocks = m["content"].as_array();
            match role {
                "user" => {
                    if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                        if !s.is_empty() {
                            messages.push(json!({"role":"user","content":s}));
                        }
                    } else if let Some(blocks) = blocks {
                        let mut text_parts = Vec::new();
                        for b in blocks {
                            let btype = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            match btype {
                                "text" => {
                                    if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                                        text_parts.push(t.to_string());
                                    }
                                }
                                "tool_result" => {
                                    if !text_parts.is_empty() {
                                        messages.push(
                                            json!({"role":"user","content":text_parts.join("")}),
                                        );
                                        text_parts.clear();
                                    }
                                    let tool_use_id =
                                        b.get("tool_use_id").and_then(|x| x.as_str()).unwrap_or("");
                                    let content = block_content_to_text(b.get("content"));
                                    messages.push(json!({"role":"tool","tool_call_id":tool_use_id,"content":content}));
                                }
                                _ => {}
                            }
                        }
                        if !text_parts.is_empty() {
                            messages.push(json!({"role":"user","content":text_parts.join("")}));
                        }
                    }
                }
                "assistant" => {
                    let mut text_parts = Vec::new();
                    let mut reasoning_parts = Vec::new();
                    let mut signatures: Vec<String> = Vec::new();
                    let mut tool_calls = Vec::new();
                    if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                        if !s.is_empty() {
                            text_parts.push(s.to_string());
                        }
                    } else if let Some(blocks) = blocks {
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
                                    // 提取 signature 用于多轮 thinking 完整性
                                    // 优先编码为 envelope，便于跨协议（Chat→Responses）透传
                                    // 失败时（无 signature 等）回退到原字符串透传
                                    if let Some(s) = b.get("signature").and_then(|x| x.as_str()) {
                                        if !s.is_empty() {
                                            if let Some(envelope) =
                                                reasoning_bridge::encode_anthropic_thinking(b)
                                            {
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
                                    // 服务端工具调用历史，转为文本说明
                                    let name = b
                                        .get("name")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("server_tool");
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
                                    let id = b.get("id").cloned().unwrap_or(json!(""));
                                    let name = b.get("name").cloned().unwrap_or(json!(""));
                                    let input = b.get("input").cloned().unwrap_or(json!({}));
                                    let args = serde_json::to_string(&input).unwrap_or_default();
                                    tool_calls.push(json!({
                                        "id": id, "type":"function",
                                        "function":{"name":name,"arguments":args}
                                    }));
                                }
                                _ => {}
                            }
                        }
                    }
                    if !text_parts.is_empty()
                        || !tool_calls.is_empty()
                        || !reasoning_parts.is_empty()
                    {
                        let mut msg = json!({"role":"assistant"});
                        msg["content"] = if text_parts.is_empty() {
                            json!("")
                        } else {
                            json!(text_parts.join(""))
                        };
                        if !reasoning_parts.is_empty() {
                            msg["reasoning_content"] = json!(reasoning_parts.join("\n"));
                        }
                        if !signatures.is_empty() {
                            // 多个 thinking 块时用换行分隔 signature
                            msg["reasoning_signature"] = json!(signatures.join("\n"));
                        }
                        if !tool_calls.is_empty() {
                            msg["tool_calls"] = json!(tool_calls);
                        }
                        messages.push(msg);
                    }
                }
                _ => {}
            }
        }
    }

    let tools = src.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .map(|t| {
                json!({
                    "type":"function",
                    "function":{
                        "name": t["name"],
                        "description": t["description"],
                        // 规范化 JSON Schema：补 type/properties，删 format:uri
                        "parameters": clean_schema(t["input_schema"].clone()),
                    }
                })
            })
            .collect::<Vec<_>>()
    });

    let mut out = json!({
        "model": src["model"],
        "messages": messages,
        "stream": src.get("stream").cloned().unwrap_or(json!(false)),
    });
    if let Some(mt) = src.get("max_tokens") {
        out["max_tokens"] = mt.clone();
    }
    if let Some(t) = tools {
        out["tools"] = json!(t);
    }
    if let Some(tc) = src.get("tool_choice") {
        out["tool_choice"] = map_tool_choice_anthropic_to_chat(tc);
    }
    if let Some(t) = src.get("temperature") {
        out["temperature"] = t.clone();
    }
    if let Some(t) = src.get("top_p") {
        out["top_p"] = t.clone();
    }
    if let Some(ss) = src.get("stop_sequences") {
        out["stop"] = ss.clone();
    }
    // thinking → reasoning_effort（Anthropic 客户端 → Chat 上游）
    // Anthropic 的 thinking 配置需要转为 Chat 的 reasoning_effort 字段，
    // 否则 Chat 上游不知道要启用推理模式，thinking 内容不会返回。
    if let Some(thinking) = src.get("thinking") {
        let thinking_type = thinking.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if thinking_type == "enabled" || thinking_type == "adaptive" {
            // 根据 budget_tokens 映射 effort 级别
            let budget = thinking.get("budget_tokens").and_then(|b| b.as_u64()).unwrap_or(8192);
            let effort = if budget < 4000 {
                "low"
            } else if budget < 16000 {
                "medium"
            } else {
                "high"
            };
            // adaptive 模式映射为 high
            let effort = if thinking_type == "adaptive" { "high" } else { effort };
            out["reasoning_effort"] = json!(effort);
        }
    }
    // output_config.effort → reasoning_effort（Anthropic 新版 effort 字段）
    if out.get("reasoning_effort").is_none() {
        if let Some(effort) = src.pointer("/output_config/effort").and_then(|v| v.as_str()) {
            // Anthropic "max" → Chat/OpenAI "xhigh"
            let mapped = if effort == "max" { "xhigh" } else { effort };
            out["reasoning_effort"] = json!(mapped);
        }
    }
    out
}

// ---------- Chat -> Responses ----------

pub fn chat_to_responses_req(src: &Value) -> Value {
    let mut instructions = Vec::new();
    let mut input = Vec::<Value>::new();
    let messages_in = src["messages"].as_array().cloned().unwrap_or_default();

    for m in messages_in {
        let role = m["role"].as_str().unwrap_or("user");
        match role {
            "system" => {
                let text = chat_content_to_text(&m["content"]);
                // 剥离 Claude Code 注入的 billing header（仅开头第一行）
                let text = strip_leading_anthropic_billing_header(&text);
                if !text.is_empty() {
                    instructions.push(text.to_string());
                }
            }
            "user" => {
                let blocks = chat_content_to_responses_blocks(&m["content"], "user");
                if !blocks.is_empty() {
                    input.push(json!({
                        "type":"message","role":"user","content":blocks
                    }));
                }
            }
            "assistant" => {
                // reasoning_content → reasoning item（携带 summary，保证多轮推理回传）
                if let Some(rc) = m.get("reasoning_content").and_then(|x| x.as_str()) {
                    if !rc.is_empty() {
                        let signature = m
                            .get("reasoning_signature")
                            .and_then(|x| x.as_str())
                            .unwrap_or("");
                        // 检测 envelope：跨协议（Anthropic→Chat→Responses）保真。
                        // envelope 字符串直接作为 encrypted_content 透传，不解码——
                        // 这样后续 Responses→Chat→Anthropic 时 reasoning_bridge 还能
                        // 完整还原原 Anthropic thinking 块（含 signature）。
                        let encrypted = if reasoning_bridge::is_envelope(signature) {
                            Some(signature.to_string())
                        } else if !signature.is_empty() {
                            // 旧版兼容：非 envelope signature 字符串也透传为 encrypted_content
                            Some(signature.to_string())
                        } else {
                            None
                        };
                        let mut item = json!({
                            "type":"reasoning",
                            "summary":[{"type":"summary_text","text":rc}]
                        });
                        if let Some(enc) = encrypted {
                            item["encrypted_content"] = json!(enc);
                        }
                        input.push(item);
                    }
                }
                let blocks = chat_content_to_responses_blocks(&m["content"], "assistant");
                if !blocks.is_empty() {
                    input.push(json!({
                        "type":"message","role":"assistant","content":blocks
                    }));
                }
                if let Some(tcs) = m["tool_calls"].as_array() {
                    for tc in tcs {
                        let call_id = tc["id"].as_str().unwrap_or("");
                        let name = tc["function"]["name"].as_str().unwrap_or("");
                        let args = tc["function"]["arguments"].as_str().unwrap_or("{}");
                        input.push(json!({
                            "type":"function_call",
                            "call_id": call_id,
                            "name": name,
                            "arguments": args
                        }));
                    }
                }
            }
            "tool" => {
                let call_id = m["tool_call_id"].as_str().unwrap_or("");
                let output = m["content"].as_str().unwrap_or("");
                input.push(json!({
                    "type":"function_call_output",
                    "call_id": call_id,
                    "output": output
                }));
            }
            _ => {}
        }
    }

    let tools = src.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|t| {
                let f = t.get("function")?;
                Some(json!({
                    "type":"function",
                    "name": f["name"],
                    "description": f["description"],
                    "parameters": f["parameters"],
                }))
            })
            .collect::<Vec<_>>()
    });

    let mut out = json!({
        "model": src["model"],
        "input": input,
        "stream": src.get("stream").cloned().unwrap_or(json!(false)),
    });
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions.join("\n\n"));
    }
    if let Some(t) = tools {
        out["tools"] = json!(t);
    }
    if let Some(tc) = src.get("tool_choice") {
        out["tool_choice"] = map_tool_choice_chat_to_responses(tc);
    }
    if let Some(t) = src.get("temperature") {
        out["temperature"] = t.clone();
    }
    if let Some(t) = src.get("top_p") {
        out["top_p"] = t.clone();
    }
    // reasoning_effort → reasoning.effort（Chat 客户端 → Responses 上游）
    if let Some(effort) = src.get("reasoning_effort").and_then(|v| v.as_str()) {
        if !effort.is_empty() {
            out["reasoning"] = json!({"effort": effort, "summary": "auto"});
        }
    }
    // 跨协议方向：剥离 cache_control
    strip_cache_control(&mut out);
    out
}

// ---------- Responses -> Chat ----------

pub fn responses_to_chat_req(src: &Value) -> Result<Value, AppError> {
    let mut messages = Vec::<Value>::new();

    if let Some(ins) = src.get("instructions").and_then(|x| x.as_str()) {
        if !ins.is_empty() {
            messages.push(json!({"role":"system","content":ins}));
        }
    }

    match src.get("input") {
        Some(Value::String(s)) => {
            messages.push(json!({"role":"user","content":s}));
        }
        Some(Value::Array(arr)) => {
            for item in arr {
                let itype = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match itype {
                    "message" => {
                        let role = item.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                        let text = responses_content_to_text(item.get("content"), role);
                        if !text.is_empty() {
                            messages.push(json!({"role": role, "content": text}));
                        }
                    }
                    "reasoning" => {
                        // Responses 上轮推理摘要回传，转成 assistant 的 reasoning_content
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
                            let mut msg = json!({"role":"assistant","content":"","reasoning_content":summary_text});
                            // 把完整 reasoning item（含 encrypted_content）编码为 envelope
                            // 放入 reasoning_signature，便于后续 Chat→Responses 跨协议保真
                            if let Some(envelope) = reasoning_bridge::encode_openai_reasoning(item)
                            {
                                msg["reasoning_signature"] = json!(envelope);
                            }
                            messages.push(msg);
                        }
                    }
                    "function_call" => {
                        let id = item.get("call_id").cloned().unwrap_or(json!(""));
                        let name = item.get("name").cloned().unwrap_or(json!(""));
                        let args = item.get("arguments").cloned().unwrap_or(json!("{}"));
                        let pushed = if let Some(last) = messages.last_mut() {
                            if last.get("role").and_then(|r| r.as_str()) == Some("assistant")
                                && last.get("tool_calls").is_some()
                            {
                                last["tool_calls"].as_array_mut().unwrap().push(json!({
                                    "id": id, "type":"function",
                                    "function":{"name":name,"arguments":args}
                                }));
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if !pushed {
                            messages.push(json!({
                                "role":"assistant","content":null,
                                "tool_calls":[{
                                    "id": id, "type":"function",
                                    "function":{"name":name,"arguments":args}
                                }]
                            }));
                        }
                    }
                    "function_call_output" => {
                        let call_id = item.get("call_id").and_then(|x| x.as_str()).unwrap_or("");
                        let output = item.get("output").and_then(|x| x.as_str()).unwrap_or("");
                        messages
                            .push(json!({"role":"tool","tool_call_id":call_id,"content":output}));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }

    let tools = src
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|arr| -> Result<Vec<Value>, AppError> {
            let mut out: Vec<Value> = Vec::new();
            let mut tool_search_seen = false;
            for t in arr {
                let tool_type = t.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match tool_type {
                    "function" => {
                        out.push(json!({
                            "type":"function",
                            "function":{
                                "name": t["name"],
                                "description": t["description"],
                                "parameters": clean_schema(t["parameters"].clone()),
                            }
                        }));
                    }
                    // namespace 工具：摊平为多个 function 工具
                    "namespace" => {
                        let ns_name = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let children: Option<&Vec<Value>> = t
                            .get("tools")
                            .and_then(|t| t.as_array())
                            .or_else(|| t.get("children").and_then(|c| c.as_array()));
                        if let Some(children) = children {
                            for (flat_tool, _owner) in
                                tools::flatten_namespace_children(ns_name, children)
                            {
                                out.push(flat_tool);
                            }
                        }
                    }
                    // custom/freeform 工具：降级为 function 工具（input: string schema）
                    "custom" | "freeform" => {
                        out.push(tools::custom_tool_to_function(t));
                    }
                    // tool_search 服务端工具：代理为同名 function 工具
                    "tool_search" => {
                        if !tool_search_seen {
                            out.push(tools::tool_search_proxy_function());
                            tool_search_seen = true;
                        }
                    }
                    // 其它服务端工具（web_search / file_search 等）丢弃：
                    // Chat 上游无对应能力，保留会触发 400
                    _ => {}
                }
            }
            Ok(out)
        })
        .transpose()?;

    let mut out = json!({
        "model": src["model"],
        "messages": messages,
        "stream": src.get("stream").cloned().unwrap_or(json!(false)),
    });
    if let Some(t) = tools {
        out["tools"] = json!(t);
    }
    if let Some(tc) = src.get("tool_choice") {
        out["tool_choice"] = map_tool_choice_responses_to_chat(tc);
    }
    if let Some(t) = src.get("temperature") {
        out["temperature"] = t.clone();
    }
    if let Some(t) = src.get("top_p") {
        out["top_p"] = t.clone();
    }
    // reasoning.effort → reasoning_effort（Responses 客户端 → Chat 上游）
    if let Some(effort) = src.pointer("/reasoning/effort").and_then(|v| v.as_str()) {
        if !effort.is_empty() {
            out["reasoning_effort"] = json!(effort);
        }
    }
    Ok(out)
}
