//! 协议转换：请求体 + 非流式响应体。
//!
//! 三种协议两两转换，共 3 对核心转换：
//!   Chat ⇄ Responses、Chat ⇄ Anthropic、Responses ⇄ Anthropic（经 Chat 两级跳转）。
//! 流式转换见 stream.rs。

use serde_json::{json, Value};

use crate::config::Protocol;
use crate::error::AppError;

// ===================== 分发 =====================

/// 请求体转换：src 协议 -> dst 协议
///   src = consumer 协议, dst = provider 协议
pub fn req_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    match (s, d) {
        _ if s == d => Ok(src.clone()),
        (Protocol::Chat, Protocol::Responses) => Ok(chat_to_responses_req(src)),
        (Protocol::Responses, Protocol::Chat) => Ok(responses_to_chat_req(src)),
        (Protocol::Chat, Protocol::Anthropic) => Ok(chat_to_anthropic_req(src)),
        (Protocol::Anthropic, Protocol::Chat) => Ok(anthropic_to_chat_req(src)),
        (Protocol::Responses, Protocol::Anthropic) => {
            // responses -> chat -> anthropic
            let chat = responses_to_chat_req(src);
            Ok(chat_to_anthropic_req(&chat))
        }
        (Protocol::Anthropic, Protocol::Responses) => {
            let chat = anthropic_to_chat_req(src);
            Ok(chat_to_responses_req(&chat))
        }
        _ => Ok(src.clone()),
    }
}

/// 非流式响应体转换：src 协议 -> dst 协议
///   src = provider 协议, dst = consumer 协议
pub fn resp_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    match (s, d) {
        _ if s == d => Ok(src.clone()),
        (Protocol::Responses, Protocol::Chat) => Ok(responses_to_chat_resp(src)),
        (Protocol::Chat, Protocol::Responses) => Ok(chat_to_responses_resp(src)),
        (Protocol::Anthropic, Protocol::Chat) => Ok(anthropic_to_chat_resp(src)),
        (Protocol::Chat, Protocol::Anthropic) => Ok(chat_to_anthropic_resp(src)),
        (Protocol::Anthropic, Protocol::Responses) => {
            let chat = anthropic_to_chat_resp(src);
            Ok(chat_to_responses_resp(&chat))
        }
        (Protocol::Responses, Protocol::Anthropic) => {
            let chat = responses_to_chat_resp(src);
            Ok(chat_to_anthropic_resp(&chat))
        }
        _ => Ok(src.clone()),
    }
}

// ===================== 通用 helper =====================

/// 简单生成一个本地唯一 id 后缀
fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}")
}

/// 当前 Unix 时间戳（秒），用于 chat completion 的 created 字段
fn created_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// chat content（string 或 [{type:text,text}]）-> 纯文本
fn chat_content_to_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
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

/// anthropic/tool_result 的 content（string 或 [{type:text,text}]）-> 纯文本
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

/// responses content（按 role 选 input_text/output_text）-> 纯文本
fn responses_content_to_text(content: Option<&Value>, role: &str) -> String {
    let want = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
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

// ---------- stop_reason / finish_reason 映射 ----------

pub fn map_stop_reason_anthropic_to_chat(sr: &str) -> String {
    match sr {
        "end_turn" | "stop_sequence" => "stop".into(),
        "tool_use" => "tool_calls".into(),
        "max_tokens" => "length".into(),
        _ => sr.into(),
    }
}

pub fn map_finish_reason_chat_to_anthropic(fr: &str) -> String {
    match fr {
        "stop" => "end_turn".into(),
        "tool_calls" => "tool_use".into(),
        "length" => "max_tokens".into(),
        _ => fr.into(),
    }
}

fn map_status_responses_to_chat(s: &str) -> String {
    match s {
        "completed" => "stop".into(),
        "incomplete" => "length".into(),
        _ => "stop".into(),
    }
}

// ---------- tool_choice 映射 ----------

fn map_tool_choice_chat_to_anthropic(tc: &Value) -> Value {
    match tc {
        Value::String(s) => match s.as_str() {
            "auto" => json!({"type":"auto"}),
            "none" => json!({"type":"none"}),
            "required" => json!({"type":"any"}),
            _ => json!({"type":"auto"}),
        },
        Value::Object(_) => {
            if let Some(name) = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
            {
                json!({"type":"tool","name":name})
            } else {
                json!({"type":"auto"})
            }
        }
        _ => json!({"type":"auto"}),
    }
}

fn map_tool_choice_anthropic_to_chat(tc: &Value) -> Value {
    match tc {
        Value::String(s) => json!(s),
        Value::Object(_) => {
            let t = tc.get("type").and_then(|x| x.as_str()).unwrap_or("");
            match t {
                "auto" => json!("auto"),
                "any" => json!("required"),
                "tool" => {
                    let name = tc.get("name").cloned().unwrap_or(json!(""));
                    json!({"type":"function","function":{"name":name}})
                }
                _ => json!("auto"),
            }
        }
        _ => json!("auto"),
    }
}

fn map_tool_choice_chat_to_responses(tc: &Value) -> Value {
    match tc {
        Value::String(_) => tc.clone(),
        Value::Object(_) => {
            if let Some(name) = tc.get("function").and_then(|f| f.get("name")).cloned() {
                json!({"type":"function","name":name})
            } else {
                tc.clone()
            }
        }
        _ => tc.clone(),
    }
}

fn map_tool_choice_responses_to_chat(tc: &Value) -> Value {
    match tc {
        Value::String(_) => tc.clone(),
        Value::Object(_) => {
            if tc.get("type").and_then(|t| t.as_str()) == Some("function") {
                if let Some(name) = tc.get("name").cloned() {
                    return json!({"type":"function","function":{"name":name}});
                }
            }
            tc.clone()
        }
        _ => tc.clone(),
    }
}

// ===================== 请求体转换 =====================

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
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "user" => {
                let text = chat_content_to_text(&m["content"]);
                if !text.is_empty() {
                    push_anthropic_user(
                        &mut messages,
                        json!({"role":"user","content":[{"type":"text","text":text}]}),
                    );
                }
            }
            "assistant" => {
                let mut blocks = Vec::new();
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
                    "input_schema": f["parameters"],
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
    out
}

/// 合并相邻 user 消息（Anthropic 要求 role 交替；连续 tool_result 合并进同一 user）
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
                    // content 可能是字符串或数组，字符串直接作为文本
                    if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                        if !s.is_empty() {
                            messages.push(json!({"role":"user","content":s}));
                        }
                    } else if let Some(blocks) = blocks {
                        // 按原始顺序处理：text 块和 tool_result 块交错 push
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
                                    // 先 flush 累积的 text，保持原始顺序
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
                    let mut tool_calls = Vec::new();
                    // content 可能是字符串或数组，字符串直接作为文本
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
                                    // 保留式思考：thinking block -> reasoning_content
                                    if let Some(t) = b.get("thinking").and_then(|x| x.as_str()) {
                                        reasoning_parts.push(t.to_string());
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
                        // 思考内容映射到 reasoning_content（保留式思考回传）
                        if !reasoning_parts.is_empty() {
                            msg["reasoning_content"] = json!(reasoning_parts.join(""));
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
                        "parameters": t["input_schema"],
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
    // max_tokens 透传（chat 协议也用 max_tokens）
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
    // stop_sequences -> stop
    if let Some(ss) = src.get("stop_sequences") {
        out["stop"] = ss.clone();
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
                if !text.is_empty() {
                    instructions.push(text);
                }
            }
            "user" => {
                let text = chat_content_to_text(&m["content"]);
                input.push(json!({
                    "type":"message","role":"user",
                    "content":[{"type":"input_text","text":text}]
                }));
            }
            "assistant" => {
                let text = chat_content_to_text(&m["content"]);
                if !text.is_empty() {
                    input.push(json!({
                        "type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":text}]
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
    out
}

// ---------- Responses -> Chat ----------
pub fn responses_to_chat_req(src: &Value) -> Value {
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

    let tools = src.get("tools").and_then(|t| t.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|t| {
                if t.get("type").and_then(|x| x.as_str()) == Some("function") {
                    Some(json!({
                        "type":"function",
                        "function":{
                            "name": t["name"],
                            "description": t["description"],
                            "parameters": t["parameters"],
                        }
                    }))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    });

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
    out
}

// ===================== 响应体转换 =====================

// ---------- Anthropic -> Chat 响应 ----------
pub fn anthropic_to_chat_resp(src: &Value) -> Value {
    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
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
                    // 思考内容 -> chat 协议的 reasoning_content
                    if let Some(t) = b.get("thinking").and_then(|x| x.as_str()) {
                        reasoning_parts.push(t.to_string());
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
    // 思考内容映射到 reasoning_content
    if !reasoning_parts.is_empty() {
        message["reasoning_content"] = json!(reasoning_parts.join(""));
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
        if let (Some(a), Some(b)) = (
            u.get("input_tokens").and_then(|x| x.as_u64()),
            u.get("output_tokens").and_then(|x| x.as_u64()),
        ) {
            usage["total_tokens"] = json!(a + b);
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
    // 确保必要字段存在（responses 协议要求 input_tokens），缺失时补 0
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
