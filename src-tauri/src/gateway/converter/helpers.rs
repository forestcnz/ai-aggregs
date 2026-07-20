//! 协议转换的通用 helper：ID/时间戳生成、content 文本提取、stop/finish_reason 映射、tool_choice 映射。
//!
//! 这些函数被 `request.rs` 和 `response.rs` 共用，部分（`rand_id`/`created_now`/
//! `map_stop_reason_anthropic_to_chat`/`map_finish_reason_chat_to_anthropic`）也通过
//! `super::mod.rs` 再导出给 `stream` 模块使用。

use serde_json::{json, Value};

// ===================== ID / 时间戳 =====================

pub fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}")
}

pub fn created_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===================== content 文本提取 =====================

pub(super) fn chat_content_to_text(content: &Value) -> String {
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

/// 把 Chat 协议下的 content（string 或 part array）转为 Anthropic 块数组，
/// 同时处理 text 与 image_url（data:URL 解码为 base64，http(s) 走 url 源）。
pub(super) fn chat_content_to_anthropic_blocks(content: &Value) -> Vec<Value> {
    let mut blocks = Vec::new();
    match content {
        Value::String(s) => {
            if !s.is_empty() {
                blocks.push(json!({"type":"text","text":s}));
            }
        }
        Value::Array(arr) => {
            for b in arr {
                let t = b.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                match t {
                    "text" => {
                        if let Some(txt) = b.get("text").and_then(|x| x.as_str()) {
                            if !txt.is_empty() {
                                blocks.push(json!({"type":"text","text":txt}));
                            }
                        }
                    }
                    "image_url" => {
                        let url = b
                            .get("image_url")
                            .and_then(|iu| iu.get("url"))
                            .and_then(|u| u.as_str())
                            .unwrap_or("");
                        if let Some(img) = url_to_anthropic_image(url) {
                            blocks.push(img);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    blocks
}

/// data:URL → base64 image 块；http(s) URL → url image 块（Anthropic 新版支持）。
pub(super) fn url_to_anthropic_image(url: &str) -> Option<Value> {
    if url.is_empty() {
        return None;
    }
    if let Some(rest) = url.strip_prefix("data:") {
        // 格式：data:<media_type>;base64,<data>
        if let (Some(semi), Some(comma)) = (rest.find(';'), rest.find(',')) {
            if semi < comma && rest[semi..comma].eq_ignore_ascii_case(";base64") {
                let media_type = &rest[..semi];
                let data = &rest[comma + 1..];
                if !media_type.is_empty() && !data.is_empty() {
                    return Some(json!({
                        "type":"image",
                        "source":{"type":"base64","media_type":media_type,"data":data}
                    }));
                }
            }
        }
        None
    } else {
        Some(json!({
            "type":"image",
            "source":{"type":"url","url":url}
        }))
    }
}

/// 把 Chat 协议下的 content 转为 Responses 协议的 content part 数组，
/// user 用 input_text/input_image，assistant 用 output_text。
pub(super) fn chat_content_to_responses_blocks(content: &Value, role: &str) -> Vec<Value> {
    let text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let mut blocks = Vec::new();
    match content {
        Value::String(s) => {
            if !s.is_empty() {
                blocks.push(json!({"type":text_type,"text":s}));
            }
        }
        Value::Array(arr) => {
            for b in arr {
                let t = b.get("type").and_then(|x| x.as_str()).unwrap_or("text");
                match t {
                    "text" => {
                        if let Some(txt) = b.get("text").and_then(|x| x.as_str()) {
                            if !txt.is_empty() {
                                blocks.push(json!({"type":text_type,"text":txt}));
                            }
                        }
                    }
                    "image_url" => {
                        let url = b
                            .get("image_url")
                            .and_then(|iu| iu.get("url"))
                            .and_then(|u| u.as_str())
                            .unwrap_or("");
                        if !url.is_empty() {
                            blocks.push(json!({"type":"input_image","image_url":url}));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    blocks
}

pub(super) fn block_content_to_text(content: Option<&Value>) -> String {
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

pub(super) fn responses_content_to_text(content: Option<&Value>, role: &str) -> String {
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
        "end_turn" | "stop_sequence" | "pause_turn" => "stop".into(),
        "tool_use" => "tool_calls".into(),
        "max_tokens" | "model_context_window_exceeded" => "length".into(),
        "refusal" | "unsafe_content" => "content_filter".into(),
        _ => sr.into(),
    }
}

pub fn map_finish_reason_chat_to_anthropic(fr: &str) -> String {
    match fr {
        "stop" => "end_turn".into(),
        "tool_calls" => "tool_use".into(),
        "length" => "max_tokens".into(),
        "content_filter" => "refusal".into(),
        "function_call" => "tool_use".into(),
        _ => fr.into(),
    }
}

pub(super) fn map_status_responses_to_chat(s: &str) -> String {
    match s {
        "completed" => "stop".into(),
        "incomplete" => "length".into(),
        // cancelled / failed 没有 chat finish_reason 对应，回退到 stop 避免客户端报错
        _ => "stop".into(),
    }
}

// ---------- tool_choice 映射 ----------

pub(super) fn map_tool_choice_chat_to_anthropic(tc: &Value) -> Value {
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

pub(super) fn map_tool_choice_anthropic_to_chat(tc: &Value) -> Value {
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

pub(super) fn map_tool_choice_chat_to_responses(tc: &Value) -> Value {
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

pub(super) fn map_tool_choice_responses_to_chat(tc: &Value) -> Value {
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
