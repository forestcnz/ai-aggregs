//! 响应体转换：4 个方向的非流式响应体转换函数。
//!
//! - Chat ↔ Anthropic：`chat_to_anthropic_resp` / `anthropic_to_chat_resp`
//! - Chat ↔ Responses：`chat_to_responses_resp` / `responses_to_chat_resp`

use serde_json::{json, Value};

use crate::gateway::converter::helpers::*;

// ---------- Anthropic -> Chat 响应 ----------

pub fn anthropic_to_chat_resp(src: &Value) -> Value {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut signatures: Vec<String> = Vec::new();
    let mut tool_calls = Vec::new();
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
                    // 提取 signature，保证多轮 thinking 完整性
                    if let Some(s) = b.get("signature").and_then(|x| x.as_str()) {
                        if !s.is_empty() {
                            signatures.push(s.to_string());
                        }
                    }
                }
                "redacted_thinking" => {
                    // 不可读的 thinking，用占位标记保留语义
                    reasoning_parts.push("[redacted_thinking]".to_string());
                }
                "server_tool_use" => {
                    // 服务端工具调用，转为文本说明（Chat 协议无对应概念）
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
                    // 服务端工具结果，转为简短文本占位
                    text_parts.push(format!("[{btype}]"));
                }
                "fallback" => {
                    // 服务端 fallback 块（refusal 后的替代输出）
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
    let stop_reason = src
        .get("stop_reason")
        .and_then(|x| x.as_str())
        .unwrap_or("end_turn");
    let finish_reason = map_stop_reason_anthropic_to_chat(stop_reason);

    let mut message = json!({"role":"assistant"});
    message["content"] = if text_parts.is_empty() {
        json!("")
    } else {
        json!(text_parts.join(""))
    };
    if !reasoning_parts.is_empty() {
        message["reasoning_content"] = json!(reasoning_parts.join("\n"));
    }
    // 多个 thinking 块时合并 signature（用换行分隔）
    if !signatures.is_empty() {
        message["reasoning_signature"] = json!(signatures.join("\n"));
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    // usage：Anthropic 的 input + cache_creation + cache_read 三者合计对应 Chat 的 prompt_tokens
    let mut usage = json!({});
    if let Some(u) = src.get("usage") {
        let input_tokens = u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        let cache_creation = u
            .get("cache_creation_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let cache_read = u
            .get("cache_read_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0);
        let prompt_total = input_tokens + cache_creation + cache_read;
        let output_tokens = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
        usage["prompt_tokens"] = json!(prompt_total);
        usage["completion_tokens"] = json!(output_tokens);
        usage["total_tokens"] = json!(prompt_total + output_tokens);
        // 保留 cache 字段（非标准扩展），便于前端展示与计费对账
        if cache_creation > 0 {
            usage["cache_creation_input_tokens"] = json!(cache_creation);
        }
        if cache_read > 0 {
            usage["cache_read_input_tokens"] = json!(cache_read);
        }
    }

    json!({
        "id": src.get("id").cloned().unwrap_or(json!(format!("chatcmpl-{}", rand_id()))),
        "object": "chat.completion",
        "created": created_now(),
        "model": src.get("model").cloned().unwrap_or(json!("")),
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": finish_reason
        }],
        "usage": usage
    })
}

// ---------- Chat -> Anthropic 响应 ----------

pub fn chat_to_anthropic_resp(src: &Value) -> Value {
    let mut content = Vec::new();
    let mut stop_reason = "end_turn".to_string();
    if let Some(choice) = src
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    {
        if let Some(msg) = choice.get("message") {
            // reasoning_content → thinking 块（带头部 signature，保证多轮 thinking 完整性）
            if let Some(rc) = msg.get("reasoning_content").and_then(|x| x.as_str()) {
                if !rc.is_empty() {
                    let signature = msg
                        .get("reasoning_signature")
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    let mut block = json!({"type":"thinking","thinking":rc});
                    if !signature.is_empty() {
                        block["signature"] = json!(signature);
                    }
                    content.push(block);
                }
            }
            if let Some(t) = msg.get("content").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    content.push(json!({"type":"text","text":t}));
                }
            }
            if let Some(tcs) = msg.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in tcs {
                    let id = tc.get("id").cloned().unwrap_or(json!(""));
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .cloned()
                        .unwrap_or(json!(""));
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    let input: Value = serde_json::from_str(args).unwrap_or(json!({}));
                    content.push(json!({"type":"tool_use","id":id,"name":name,"input":input}));
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str()) {
            stop_reason = map_finish_reason_chat_to_anthropic(fr);
        }
    }

    let mut usage = json!({});
    if let Some(u) = src.get("usage") {
        if let Some(pt) = u.get("prompt_tokens") {
            usage["input_tokens"] = pt.clone();
        }
        if let Some(ct) = u.get("completion_tokens") {
            usage["output_tokens"] = ct.clone();
        }
    }

    json!({
        "id": src.get("id").cloned().unwrap_or(json!(format!("msg_{}", rand_id()))),
        "type": "message",
        "role": "assistant",
        "model": src.get("model").cloned().unwrap_or(json!("")),
        "content": content,
        "stop_reason": stop_reason,
        "usage": usage
    })
}

// ---------- Responses -> Chat 响应 ----------

pub fn responses_to_chat_resp(src: &Value) -> Value {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut signatures: Vec<String> = Vec::new();
    let mut tool_calls = Vec::new();
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
                    // reasoning summary → reasoning_content（Responses 上游 → Chat 客户端）
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
                        reasoning_parts.push(summary_text);
                    }
                    // encrypted_content → reasoning_signature（跨协议 reasoning 保真）
                    if let Some(enc) =
                        item.get("encrypted_content").and_then(|x| x.as_str())
                    {
                        if !enc.is_empty() {
                            signatures.push(enc.to_string());
                        }
                    }
                }
                "function_call" => {
                    let id = item.get("call_id").cloned().unwrap_or(json!(""));
                    let name = item.get("name").cloned().unwrap_or(json!(""));
                    let args = item.get("arguments").cloned().unwrap_or(json!("{}"));
                    tool_calls.push(json!({
                        "id": id, "type":"function",
                        "function":{"name":name,"arguments":args}
                    }));
                }
                _ => {}
            }
        }
    }

    let status = src
        .get("status")
        .and_then(|x| x.as_str())
        .unwrap_or("completed");
    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls".to_string()
    } else {
        map_status_responses_to_chat(status)
    };

    let mut message = json!({"role":"assistant"});
    message["content"] = if text_parts.is_empty() {
        json!("")
    } else {
        json!(text_parts.join(""))
    };
    if !reasoning_parts.is_empty() {
        message["reasoning_content"] = json!(reasoning_parts.join("\n"));
    }
    if !signatures.is_empty() {
        message["reasoning_signature"] = json!(signatures.join("\n"));
    }
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    let mut usage = json!({});
    if let Some(u) = src.get("usage") {
        if let Some(it) = u.get("input_tokens") {
            usage["prompt_tokens"] = it.clone();
        }
        if let Some(ot) = u.get("output_tokens") {
            usage["completion_tokens"] = ot.clone();
        }
        if let Some(tt) = u.get("total_tokens") {
            usage["total_tokens"] = tt.clone();
        }
    }

    json!({
        "id": src.get("id").cloned().unwrap_or(json!(format!("chatcmpl-{}", rand_id()))),
        "object": "chat.completion",
        "created": created_now(),
        "model": src.get("model").cloned().unwrap_or(json!("")),
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": finish_reason
        }],
        "usage": usage
    })
}

// ---------- Chat -> Responses 响应 ----------

pub fn chat_to_responses_resp(src: &Value) -> Value {
    let mut output = Vec::new();
    if let Some(choice) = src
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    {
        if let Some(msg) = choice.get("message") {
            // reasoning_content → reasoning output item（Chat 上游 → Responses 客户端）
            // 必须在 message item 之前输出，保持与 OpenAI 原生 Responses 行为一致
            if let Some(rc) = msg.get("reasoning_content").and_then(|x| x.as_str()) {
                if !rc.is_empty() {
                    let mut reasoning_item = json!({
                        "type":"reasoning",
                        "summary":[{"type":"summary_text","text":rc}]
                    });
                    // reasoning_signature → encrypted_content（跨协议 reasoning 保真）
                    if let Some(sig) = msg.get("reasoning_signature").and_then(|x| x.as_str()) {
                        if !sig.is_empty() {
                            reasoning_item["encrypted_content"] = json!(sig);
                        }
                    }
                    output.push(reasoning_item);
                }
            }
            if let Some(t) = msg.get("content").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    output.push(json!({
                        "type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":t}]
                    }));
                }
            }
            if let Some(tcs) = msg.get("tool_calls").and_then(|x| x.as_array()) {
                for tc in tcs {
                    let id = tc.get("id").cloned().unwrap_or(json!(""));
                    let name = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .cloned()
                        .unwrap_or(json!(""));
                    let args = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("{}");
                    output.push(json!({
                        "type":"function_call","call_id":id,"name":name,"arguments":args
                    }));
                }
            }
        }
    }

    let mut usage = json!({});
    if let Some(u) = src.get("usage") {
        if let Some(pt) = u.get("prompt_tokens") {
            usage["input_tokens"] = pt.clone();
        }
        if let Some(ct) = u.get("completion_tokens") {
            usage["output_tokens"] = ct.clone();
        }
        if let Some(tt) = u.get("total_tokens") {
            usage["total_tokens"] = tt.clone();
        }
    }
    if usage.get("input_tokens").is_none() {
        usage["input_tokens"] = json!(0);
    }
    if usage.get("output_tokens").is_none() {
        usage["output_tokens"] = json!(0);
    }
    if usage.get("total_tokens").is_none() {
        if let (Some(a), Some(b)) = (
            usage.get("input_tokens").and_then(|x| x.as_u64()),
            usage.get("output_tokens").and_then(|x| x.as_u64()),
        ) {
            usage["total_tokens"] = json!(a + b);
        } else {
            usage["total_tokens"] = json!(0);
        }
    }

    json!({
        "id": src.get("id").cloned().unwrap_or(json!(format!("resp_{}", rand_id()))),
        "object": "response",
        "model": src.get("model").cloned().unwrap_or(json!("")),
        "output": output,
        "status": "completed",
        "usage": usage
    })
}
