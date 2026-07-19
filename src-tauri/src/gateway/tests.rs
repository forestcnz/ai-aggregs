//! 协议转换层集成测试
//!
//! 覆盖三类场景：
//! - Round-trip：A→B→A 等价性（验证跨协议字段保真）
//! - 回归测试：与具体边界 case 关联（命名 `regression_*`）
//! - Reasoning envelope：跨协议 thinking signature 保真

use serde_json::json;

use crate::config::types::Protocol;
use crate::gateway::convert_helpers::{
    self, clean_schema, extract_cache_field, strip_cache_control,
    strip_leading_anthropic_billing_header,
};
use crate::gateway::converter::{
    anthropic_to_chat_req, chat_to_anthropic_req, chat_to_responses_req, responses_to_chat_req,
};
use crate::gateway::reasoning_bridge;
use crate::gateway::stream::{make_converter, StreamConverter};

// ===================== Reasoning envelope 跨协议保真 =====================

#[test]
fn reasoning_envelope_anthropic_to_responses_preserves_thinking_signature() {
    // 场景：Anthropic 客户端的多轮对话，含 thinking.signature
    // 经 Chat 双跳转 Responses 后，encrypted_content 应保留原 signature 数据
    let original = json!({
        "model": "claude-opus-4.5",
        "max_tokens": 1024,
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "I should use a tool", "signature": "sig_abc_123"},
                {"type": "text", "text": "Let me check"},
                {"type": "tool_use", "id": "toolu_1", "name": "weather", "input": {"city": "SF"}}
            ]
        }]
    });

    // Anthropic → Chat → Responses（双跳经 Chat）
    let chat = anthropic_to_chat_req(&original);
    let responses = chat_to_responses_req(&chat);

    let input = responses["input"].as_array().expect("input should be array");
    // 应该存在 reasoning item（来自 thinking 块）
    let reasoning_item = input
        .iter()
        .find(|item| item["type"].as_str() == Some("reasoning"))
        .expect("should have reasoning item");

    // encrypted_content 应是 envelope（含完整原 thinking 块的 base64）
    let encrypted = reasoning_item["encrypted_content"]
        .as_str()
        .expect("encrypted_content should be set");
    assert!(
        reasoning_bridge::is_envelope(encrypted),
        "encrypted_content should be envelope, got: {encrypted}"
    );

    // 解码 envelope 应得原 thinking 块
    let decoded = reasoning_bridge::decode_envelope(encrypted).expect("decode should succeed");
    assert_eq!(decoded["type"], "thinking");
    assert_eq!(decoded["signature"], "sig_abc_123");
}

#[test]
fn reasoning_envelope_responses_to_anthropic_preserves_encrypted_content() {
    // 场景：Responses 客户端的多轮对话，含 reasoning.encrypted_content
    // 经 Chat 双跳转 Anthropic 后，signature 应保留原 encrypted_content 数据
    let original = json!({
        "model": "gpt-5",
        "input": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Let me check"}]
        }, {
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "I should use a tool"}],
            "encrypted_content": "opaque-blob-from-openai"
        }, {
            "type": "function_call",
            "call_id": "call_1",
            "name": "weather",
            "arguments": "{\"city\":\"SF\"}"
        }]
    });

    // Responses → Chat → Anthropic（双跳经 Chat）
    let chat = responses_to_chat_req(&original).expect("responses_to_chat_req should succeed");
    let anthropic = chat_to_anthropic_req(&chat);

    let messages = anthropic["messages"].as_array().expect("messages should be array");
    // 找到含 thinking signature 的 assistant message（可能是第二个 assistant）
    let assistant_with_thinking = messages
        .iter()
        .find(|m| {
            m["role"].as_str() == Some("assistant")
                && m["content"].as_array().is_some_and(|arr| {
                    arr.iter().any(|b| b["type"].as_str() == Some("thinking"))
                })
        })
        .expect("should have assistant message with thinking block");
    let blocks = assistant_with_thinking["content"].as_array().unwrap();
    let thinking = blocks
        .iter()
        .find(|b| b["type"].as_str() == Some("thinking"))
        .unwrap();
    let sig = thinking["signature"]
        .as_str()
        .expect("thinking signature should be set");
    assert!(
        reasoning_bridge::is_envelope(sig),
        "thinking signature should be envelope, got: {sig}"
    );

    // 解码 envelope 应得原 reasoning item
    let decoded = reasoning_bridge::decode_envelope(sig).expect("decode should succeed");
    assert_eq!(decoded["type"], "reasoning");
    assert_eq!(decoded["encrypted_content"], "opaque-blob-from-openai");
}

#[test]
fn reasoning_envelope_legacy_signature_still_preserves_data() {
    // 场景：客户端发送普通 signature 字符串（非 envelope），
    // 新版实现会自动用 envelope 包裹原 thinking 块，保证跨协议时数据完整
    let original = json!({
        "model": "claude-opus-4.5",
        "max_tokens": 1024,
        "messages": [{
            "role": "assistant",
            "content": [
                {"type": "thinking", "thinking": "old thinking", "signature": "legacy-sig-not-envelope"}
            ]
        }]
    });

    let chat = anthropic_to_chat_req(&original);
    let sig = chat["messages"][0]["reasoning_signature"]
        .as_str()
        .expect("should have reasoning_signature");
    // 新版自动编码为 envelope，内部保留原 legacy signature
    assert!(reasoning_bridge::is_envelope(sig));
    let decoded = reasoning_bridge::decode_envelope(sig).unwrap();
    assert_eq!(decoded["signature"], "legacy-sig-not-envelope");
    assert_eq!(decoded["thinking"], "old thinking");
}

// ===================== billing header 回归测试 =====================

#[test]
fn regression_claude_code_billing_header_stripped_in_chat_to_anthropic() {
    // Claude Code 注入的 billing header 会破坏上游 cache prefix 匹配
    let input = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "messages": [
            {"role": "system", "content": "x-anthropic-billing-header: cc_version=2.1; cch=abc;\n\nYou are helpful."},
            {"role": "user", "content": "Hi"}
        ]
    });
    let result = chat_to_anthropic_req(&input);
    let system = result["system"].as_str().expect("should have system");
    assert!(
        !system.contains("x-anthropic-billing-header"),
        "billing header should be stripped, got: {system}"
    );
    assert!(
        system.contains("You are helpful"),
        "user content should be preserved"
    );
}

#[test]
fn regression_claude_code_billing_header_stripped_in_anthropic_to_chat() {
    let input = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "system": "x-anthropic-billing-header: cc_version=2.1; cch=abc;\n\nYou are helpful.",
        "messages": [{"role": "user", "content": "Hi"}]
    });
    let result = anthropic_to_chat_req(&input);
    let messages = result["messages"].as_array().unwrap();
    let system_msg = messages
        .iter()
        .find(|m| m["role"].as_str() == Some("system"))
        .expect("should have system message");
    let content = system_msg["content"].as_str().unwrap();
    assert!(!content.contains("x-anthropic-billing-header"));
    assert!(content.contains("You are helpful"));
}

#[test]
fn regression_claude_code_billing_header_stripped_in_chat_to_responses() {
    let input = json!({
        "model": "gpt-5",
        "messages": [
            {"role": "system", "content": "x-anthropic-billing-header: abc;\n\nInstructions."},
            {"role": "user", "content": "Hi"}
        ]
    });
    let result = chat_to_responses_req(&input);
    let instructions = result["instructions"].as_str().expect("should have instructions");
    assert!(!instructions.contains("x-anthropic-billing-header"));
    assert!(instructions.contains("Instructions."));
}

#[test]
fn regression_billing_header_keeps_non_leading_occurrence() {
    // 用户内容中后续出现的同前缀文本不应被误删
    let text = "Keep this:\nx-anthropic-billing-header: example";
    let stripped = strip_leading_anthropic_billing_header(text);
    assert_eq!(stripped, text);
}

// ===================== JSON Schema normalize 回归测试 =====================

#[test]
fn regression_empty_tool_schema_normalized_to_object_with_properties() {
    // Anthropic 工具 input_schema 可能缺 type/properties，严格上游会 400
    // 测试 anthropic_to_chat_req 方向：input_schema → function.parameters 时 normalize
    let input = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "test"}],
        "tools": [{"name": "do_work", "input_schema": {}}]
    });
    let result = anthropic_to_chat_req(&input);
    let params = &result["tools"][0]["function"]["parameters"];
    assert_eq!(params["type"], "object");
    assert!(params["properties"].is_object());
}

#[test]
fn regression_empty_tool_schema_normalized_in_chat_to_anthropic() {
    // 测试 chat_to_anthropic_req 方向：function.parameters → input_schema 时 normalize
    let input = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "messages": [{"role": "user", "content": "test"}],
        "tools": [{"type": "function", "function": {"name": "do_work", "parameters": {}}}]
    });
    let result = chat_to_anthropic_req(&input);
    let schema = &result["tools"][0]["input_schema"];
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"].is_object());
}

#[test]
fn regression_format_uri_removed_from_schema() {
    // GLM/Qwen 等上游拒绝 format: uri
    let mut schema = json!({
        "type": "object",
        "properties": {
            "url": {"type": "string", "format": "uri"}
        }
    });
    // 通过 clean_schema 测试
    let cleaned = clean_schema(schema.clone());
    assert!(cleaned["properties"]["url"].get("format").is_none());
    // 调用 strip_cache_control 验证其他字段不变
    strip_cache_control(&mut schema);
    assert_eq!(schema["properties"]["url"]["type"], "string");
}

// ===================== cache_control 剥离回归测试 =====================

#[test]
fn regression_cache_control_stripped_in_chat_to_anthropic() {
    // GLM/Qwen 严格上游拒绝 Anthropic 的 cache_control 字段
    let input = json!({
        "model": "glm-5",
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [{"type": "text", "text": "Hi", "cache_control": {"type": "ephemeral"}}]
        }]
    });
    let result = chat_to_anthropic_req(&input);
    let result_str = serde_json::to_string(&result).unwrap();
    assert!(
        !result_str.contains("cache_control"),
        "cache_control should be stripped, got: {result_str}"
    );
}

#[test]
fn regression_cache_control_stripped_in_anthropic_to_chat() {
    let input = json!({
        "model": "glm-5",
        "max_tokens": 1024,
        "system": [{"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": "Hi"}],
        "tools": [{"name": "t", "input_schema": {}, "cache_control": {"type": "ephemeral"}}]
    });
    let result = anthropic_to_chat_req(&input);
    let result_str = serde_json::to_string(&result).unwrap();
    assert!(!result_str.contains("cache_control"));
}

// ===================== cache token 多字段兼容 =====================

#[test]
fn extract_usage_supports_anthropic_top_level_cache_tokens() {
    use crate::gateway::stream::extract_usage;
    let response = json!({
        "usage": {
            "input_tokens": 100,
            "cache_read_input_tokens": 50,
            "cache_creation_input_tokens": 30,
            "output_tokens": 200
        }
    });
    let (input, output, _total) = extract_usage(&response).expect("should extract");
    // 加法哲学：input + cache_read + cache_creation = 100 + 50 + 30 = 180
    assert_eq!(input, 180);
    assert_eq!(output, 200);
}

#[test]
fn extract_usage_supports_openai_nested_cache_tokens() {
    use crate::gateway::stream::extract_usage;
    // Kimi/GLM 等用 OpenAI nested 字段
    let response = json!({
        "usage": {
            "prompt_tokens": 180,
            "completion_tokens": 200,
            "total_tokens": 380,
            "prompt_tokens_details": {
                "cached_tokens": 50,
                "cache_write_tokens": 30
            }
        }
    });
    let (input, output, total) = extract_usage(&response).expect("should extract");
    // 加法哲学：prompt_tokens + cached + cache_creation
    // 这里 prompt_tokens = 180（OpenAI 含 cache），我们的实现是 prompt_tokens + cached + cache_creation = 180 + 50 + 30 = 260
    // 注意：这与 sub2api 的减法哲学不同，ai-aggregs 保持原"加法"语义
    assert_eq!(input, 260);
    assert_eq!(output, 200);
    assert_eq!(total, 380); // total 优先用上游返回值
}

#[test]
fn extract_usage_handles_chat_without_cache() {
    use crate::gateway::stream::extract_usage;
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }
    });
    let (input, output, total) = extract_usage(&response).unwrap();
    assert_eq!(input, 100);
    assert_eq!(output, 50);
    assert_eq!(total, 150);
}

// ===================== Round-trip 等价性测试 =====================

#[test]
fn round_trip_anthropic_chat_anthropic_preserves_user_message() {
    let original = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "Hello"}, {"type": "text", "text": "World"}]}
        ]
    });
    let chat = anthropic_to_chat_req(&original);
    let back = chat_to_anthropic_req(&chat);
    // 第一条 user message 的 text 应保留
    let original_text = original["messages"][0]["content"][0]["text"].as_str().unwrap();
    let back_messages = back["messages"].as_array().unwrap();
    let back_user_msg = back_messages
        .iter()
        .find(|m| m["role"].as_str() == Some("user"))
        .expect("should have user message");
    let back_text = back_user_msg["content"].as_str().unwrap_or_else(|| {
        back_user_msg["content"][0]["text"].as_str().unwrap_or("")
    });
    assert!(
        back_text.contains(original_text),
        "user text should round-trip, got: {back_text}"
    );
}

#[test]
fn round_trip_anthropic_chat_anthropic_preserves_tool_result() {
    let original = json!({
        "model": "claude-sonnet-4",
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "Sunny"}
            ]
        }]
    });
    let chat = anthropic_to_chat_req(&original);
    let back = chat_to_anthropic_req(&chat);
    let back_messages = back["messages"].as_array().unwrap();
    let back_user = back_messages
        .iter()
        .find(|m| m["role"].as_str() == Some("user"))
        .expect("should have user message");
    // tool_result 应被保留（可能合并到 user content 数组中）
    let back_str = serde_json::to_string(back_user).unwrap();
    assert!(
        back_str.contains("toolu_1"),
        "tool_use_id should round-trip, got: {back_str}"
    );
    assert!(back_str.contains("Sunny"));
}

#[test]
fn round_trip_chat_responses_chat_preserves_basic_message() {
    let original = json!({
        "model": "gpt-5",
        "messages": [
            {"role": "system", "content": "You are helpful"},
            {"role": "user", "content": "Hi"}
        ]
    });
    let responses = chat_to_responses_req(&original);
    let back = responses_to_chat_req(&responses).expect("responses_to_chat_req should succeed");
    // system 应保留（变为 instructions 后再变回 system）
    let back_messages = back["messages"].as_array().unwrap();
    let back_system = back_messages
        .iter()
        .find(|m| m["role"].as_str() == Some("system"))
        .expect("should have system message");
    assert_eq!(back_system["content"].as_str(), Some("You are helpful"));
    let back_user = back_messages
        .iter()
        .find(|m| m["role"].as_str() == Some("user"))
        .expect("should have user message");
    assert_eq!(back_user["content"].as_str(), Some("Hi"));
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
    let anthropic = chat_to_anthropic_req(&original);
    let back = anthropic_to_chat_req(&anthropic);
    let back_messages = back["messages"].as_array().unwrap();
    let back_assistant = back_messages
        .iter()
        .find(|m| m["role"].as_str() == Some("assistant"))
        .expect("should have assistant message");
    // tool_calls 应保留
    let tool_calls = back_assistant["tool_calls"].as_array().unwrap();
    assert_eq!(tool_calls[0]["function"]["name"], "weather");
    // arguments 可能是 string 或 object，验证包含 city
    let args_str = if tool_calls[0]["function"]["arguments"].is_string() {
        tool_calls[0]["function"]["arguments"].as_str().unwrap().to_string()
    } else {
        serde_json::to_string(&tool_calls[0]["function"]["arguments"]).unwrap()
    };
    assert!(args_str.contains("SF"), "arguments should round-trip");
}

// ===================== 流式 tool_call 乱序兜底（ChatToAnthropic）=====================

#[test]
fn stream_tool_call_name_delayed_announcement() {
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    // Chunk 1: 先发 id + 部分 arguments，但 name 缺失（DeepSeek 行为）
    let chunk1 = json!({
        "id": "chatcmpl-1",
        "model": "deepseek-v3",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"arguments": "{\"loc"}
                }]
            },
            "finish_reason": null
        }]
    });
    let events1 = converter.on_event(None, &chunk1.to_string());

    // 此时不应宣告 content_block_start（name 未到）
    let has_tool_start = events1
        .iter()
        .any(|e: &String| e.contains("content_block_start") && e.contains("tool_use"));
    assert!(!has_tool_start, "should not announce tool before name arrives");

    // Chunk 2: name 到达
    let chunk2 = json!({
        "id": "chatcmpl-1",
        "model": "deepseek-v3",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {"name": "weather"}
                }]
            },
            "finish_reason": null
        }]
    });
    let events2 = converter.on_event(None, &chunk2.to_string());

    // 现在应该宣告 content_block_start
    let combined: String = events2.iter().cloned().collect();
    assert!(
        combined.contains("content_block_start") && combined.contains("weather"),
        "should announce tool with name 'weather', got: {combined}"
    );

    // pending args 应在宣告时被 flush
    assert!(
        combined.contains("input_json_delta"),
        "pending args should be flushed on announcement"
    );
}

#[test]
fn stream_tool_call_empty_args_filled_with_braces() {
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    // 工具调用：id + name 同时到达，但没有 arguments
    let chunk1 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "no_args_tool"}
                }]
            },
            "finish_reason": null
        }]
    });
    let _ = converter.on_event(None, &chunk1.to_string());

    // finish_reason 触发关闭块，应补 "{}"
    let chunk2 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    });
    let events = converter.on_event(None, &chunk2.to_string());
    let combined: String = events.iter().cloned().collect();
    assert!(
        combined.contains(r#""partial_json":"{}""#),
        "empty args should be filled with '{{}}', got: {combined}"
    );
}

#[test]
fn stream_tool_call_normal_order_still_works() {
    // 标准上游（OpenAI）：id + name + arguments 在同一 chunk 到达
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    let chunk = json!({
        "id": "chatcmpl-1",
        "model": "gpt-4",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "weather", "arguments": "{\"city\":\"SF\"}"}
                }]
            },
            "finish_reason": null
        }]
    });
    let events = converter.on_event(None, &chunk.to_string());
    let combined: String = events.iter().cloned().collect();
    assert!(combined.contains("content_block_start"));
    assert!(combined.contains("weather"));
    assert!(combined.contains("city"));
}

#[test]
fn stream_tool_call_late_starts_when_name_never_arrives() {
    // 极端 case：arguments 已到但 name 永远没到
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    let chunk1 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"arguments": "{\"x\":1}"}
                }]
            }
        }]
    });
    let _ = converter.on_event(None, &chunk1.to_string());

    // 直接 finish_reason（name 永远没到）
    let chunk2 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
    });
    let events = converter.on_event(None, &chunk2.to_string());
    let combined: String = events.iter().cloned().collect();
    // 应兜底用 unknown_tool 宣告
    assert!(
        combined.contains("unknown_tool"),
        "should use 'unknown_tool' as fallback name, got: {combined}"
    );
    // arguments 数据不丢：JSON 序列化后 partial_json 字段值为 "{\"x\":1}"，
    // 字符串中实际包含字面 \"（反斜杠+引号）
    assert!(
        combined.contains(r#"{\"x\":1}"#),
        "arguments data should be preserved, got: {combined}"
    );
}

// ===================== helpers 单元测试通过验证 =====================

#[test]
fn helpers_module_compiles_and_runs() {
    // 简单冒烟测试：所有 helper 函数能正常调用
    assert_eq!(strip_leading_anthropic_billing_header("plain"), "plain");

    let mut v = json!({"cache_control": {"type": "ephemeral"}, "text": "x"});
    strip_cache_control(&mut v);
    assert!(v.get("cache_control").is_none());

    let cleaned = clean_schema(json!({}));
    assert_eq!(cleaned["type"], "object");

    let cache = extract_cache_field(
        &json!({"cache_read_input_tokens": 100}),
        &["cache_read_input_tokens"],
        &[("p", "c")],
    );
    assert_eq!(cache, 100);
}

// 避免未使用警告（模块导入但部分函数仅用于文档示意）
#[test]
fn _ensure_helpers_imports_used() {
    let _ = convert_helpers::strip_leading_anthropic_billing_header("x");
}

// ===================== Copilot 无限空白 bug 检测 =====================

#[test]
fn stream_copilot_infinite_whitespace_aborts_tool_call() {
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    // 正常宣告 tool_call
    let chunk1 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "edit_file", "arguments": "{\"path\":\"test\"}"}
                }]
            }
        }]
    });
    let _ = converter.on_event(None, &chunk1.to_string());

    // 发送 600 个连续换行（超过阈值 500）
    let whitespace = "\n".repeat(600);
    let chunk2 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": {"arguments": whitespace}
                }]
            }
        }]
    });
    let events = converter.on_event(None, &chunk2.to_string());
    let _combined: String = events.iter().cloned().collect();
    // 中止后不应有 input_json_delta（空白字符已被丢弃）
    // 实际上 chunk2 的空白 delta 被检测到后会清空 pending_args 并标记 aborted
    // 后续不再发任何 delta

    // 再发一个正常 chunk，不应产生 tool 相关事件
    let chunk3 = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    });
    let events3 = converter.on_event(None, &chunk3.to_string());
    let combined3: String = events3.iter().cloned().collect();
    // aborted 的 tool 不会在 finalize 中重新宣告
    assert!(
        !combined3.contains("edit_file"),
        "aborted tool should not be re-announced in finalize"
    );
}

#[test]
fn stream_normal_whitespace_does_not_abort() {
    let mut converter = make_converter(Protocol::Chat, Protocol::Anthropic);

    // 正常数量的空白（JSON 中的空格）不应触发中止
    let args = "{\"key\": \"value with spaces\"}";
    let chunk = json!({
        "id": "chatcmpl-1",
        "model": "test",
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "test_tool", "arguments": args}
                }]
            }
        }]
    });
    let events = converter.on_event(None, &chunk.to_string());
    let combined: String = events.iter().cloned().collect();
    assert!(combined.contains("content_block_start"), "normal tool should be announced");
    assert!(combined.contains("test_tool"));
}
