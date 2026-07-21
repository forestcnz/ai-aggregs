//! 非流式 req/resp 与 IR 的双向映射。
//!
//! 6 个 parse 函数：协议 JSON -> `InternalRequest`/`InternalResponse`
//! 6 个 emit 函数：`InternalRequest`/`InternalResponse` -> 协议 JSON
//!
//! 协议特有处理在 parse 阶段做"中立化"（如剥离 billing header、编码 envelope），
//! 在 emit 阶段做"目标化"（如解码 envelope、规范化 schema、根据模型自适应 thinking）。
//!
//! 这层完成后，converter/dispatcher 只需 A->IR->B 单跳，消除原 Responses<->Anthropic 双跳。

pub mod chat;
pub mod anthropic;
pub mod responses;

use serde_json::{json, Value};

use crate::gateway::ir::InternalContent;

#[cfg(test)]
use crate::gateway::ir::InternalRole;

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

// ===================== re-exports（供 converter 模块使用） =====================

pub use chat::{
    parse_chat_req, emit_chat_req, parse_chat_resp, emit_chat_resp,
};
pub use anthropic::{
    parse_anthropic_req, emit_anthropic_req, parse_anthropic_resp, emit_anthropic_resp,
};
pub use responses::{
    parse_responses_req, emit_responses_req, parse_responses_resp, emit_responses_resp,
};

// ===================== tests =====================

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
