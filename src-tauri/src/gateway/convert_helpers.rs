//! 协议转换辅助工具集
//!
//! 集中实现协议转换层的小工具函数，便于复用与单元测试：
//! - `strip_leading_anthropic_billing_header`：剥离 Claude Code 注入的 billing header
//! - `clean_schema`：补全 / 规范化 tool 参数 JSON Schema
//! - `strip_cache_control`：递归剥离 Anthropic `cache_control` 字段
//! - `extract_cache_field`：兼容 OpenAI 多种 cache token 字段命名

use serde_json::{Map, Value};

// ===================== Claude Code billing header 剥离 =====================

/// Claude Code billing header 前缀（动态注入到 system 字段开头）
const ANTHROPIC_BILLING_HEADER_PREFIX: &str = "x-anthropic-billing-header:";

/// 仅剥离 `system` 字段开头的 Claude Code billing header 行。
///
/// Claude Code 客户端会在 system prompt 开头注入动态
/// `x-anthropic-billing-header: cc_version=...; cch=...;` 元数据。该值每次请求变化，
/// 会破坏上游 prompt cache prefix 匹配（cache 命中率下降到 0）。
///
/// 仅剥离**开头第一行**：用户内容中后续出现的同前缀文本不视为 billing header，
/// 避免误删用户内容。
///
/// 借鉴 cc-switch `proxy/providers/transform.rs:18`。
pub(crate) fn strip_leading_anthropic_billing_header(text: &str) -> &str {
    if !text.starts_with(ANTHROPIC_BILLING_HEADER_PREFIX) {
        return text;
    }

    let Some(line_end) = text
        .as_bytes()
        .iter()
        .position(|byte| *byte == b'\n' || *byte == b'\r')
    else {
        // 整个 system 都是 billing header 行（罕见）
        return "";
    };

    let bytes = text.as_bytes();
    let mut rest_start = line_end + 1;
    // 处理 CRLF
    if bytes[line_end] == b'\r' && bytes.get(line_end + 1) == Some(&b'\n') {
        rest_start += 1;
    }

    let rest = &text[rest_start..];
    // 跳过紧随其后的空行（billing header 后通常有空行分隔）
    if let Some(stripped) = rest.strip_prefix("\r\n") {
        stripped
    } else if let Some(stripped) = rest.strip_prefix('\n') {
        stripped
    } else if let Some(stripped) = rest.strip_prefix('\r') {
        stripped
    } else {
        rest
    }
}

// ===================== JSON Schema normalization =====================

/// 规范化 tool 参数 JSON Schema，使其符合严格上游（OpenAI Responses / 部分 Chat 兼容上游）要求。
///
/// - 根 schema 缺 `type` 时补 `type: "object"`
/// - 根 schema 缺 `properties` 时补 `properties: {}`
/// - 递归删除 `format: "uri"`（部分上游拒绝）
/// - 嵌套 schema 不强制补 type/properties（仅根级别）
///
/// 借鉴 cc-switch `proxy/providers/transform.rs:494`。
pub(crate) fn clean_schema(schema: Value) -> Value {
    clean_schema_inner(schema, true)
}

fn clean_schema_inner(mut schema: Value, is_root: bool) -> Value {
    let Some(obj) = schema.as_object_mut() else {
        return schema;
    };

    let missing_type = is_root && !obj.contains_key("type");
    if missing_type {
        obj.insert("type".to_string(), Value::String("object".to_string()));
    }
    // 根级别缺 properties 就补（不要求 type 也缺失）
    if is_root && !obj.contains_key("properties") {
        obj.insert("properties".to_string(), Value::Object(Map::new()));
    }

    // 移除 "format": "uri"（GLM/Qwen 等严格上游拒绝）
    if obj.get("format").and_then(|v| v.as_str()) == Some("uri") {
        obj.remove("format");
    }

    // 递归处理 properties 和 items（非根级别不强制补 type）
    if let Some(properties) = obj
        .get_mut("properties")
        .and_then(|v| v.as_object_mut())
    {
        // 收集 key 避免 borrow 问题
        let keys: Vec<String> = properties.keys().cloned().collect();
        for key in keys {
            if let Some(child) = properties.get_mut(&key) {
                let cloned = std::mem::take(child);
                *child = clean_schema_inner(cloned, false);
            }
        }
    }
    if let Some(items) = obj.get_mut("items") {
        let cloned = std::mem::take(items);
        *items = clean_schema_inner(cloned, false);
    }

    schema
}

// ===================== cache_control 剥离 =====================

/// 递归剥离 Value 中所有 `cache_control` 字段。
///
/// Anthropic 协议的 `cache_control: {type: "ephemeral"}` 字段在转到 Chat / Responses
/// 协议时，部分严格上游（GLM/Qwen）会因未知字段 400。仅跨协议方向调用本函数；
/// Anthropic → Anthropic 透传时**不**调用（保留 cache_control 是上游期望行为）。
///
/// 借鉴 cc-switch 回归测试 `test_regression_gh3805_no_cache_control_leak_to_openai`。
pub(crate) fn strip_cache_control(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            obj.remove("cache_control");
            for v in obj.values_mut() {
                strip_cache_control(v);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                strip_cache_control(v);
            }
        }
        _ => {}
    }
}

// ===================== cache token 多字段兼容 =====================

/// 从 usage 对象中提取 cache token，兼容多种字段命名。
///
/// OpenAI 兼容上游（Kimi/GLM/DeepSeek 等）对 cache token 的字段命名不统一：
/// - Anthropic 标准：`cache_read_input_tokens` / `cache_creation_input_tokens`（顶层）
/// - OpenAI 标准：`prompt_tokens_details.cached_tokens`（nested）
/// - OpenAI 别名 1：`prompt_tokens_details.cache_write_tokens`
/// - OpenAI 别名 2：`prompt_tokens_details.cache_creation_tokens`
/// - OpenAI 别名 3：`input_tokens_details.cache_write_tokens`
///
/// 调用方按"顶层直接字段优先于 nested 路径"的顺序提取。
pub(crate) fn extract_cache_field(
    usage: &Value,
    top_fields: &[&str],
    nested: &[(&str, &str)],
) -> u64 {
    // 顶层直接字段优先（兼容性最高）
    for field in top_fields {
        if let Some(v) = usage.get(*field).and_then(|x| x.as_u64()) {
            return v;
        }
    }
    // nested 路径 fallback
    for (parent, child) in nested {
        let path = format!("/{parent}/{child}");
        if let Some(v) = usage.pointer(&path).and_then(|x| x.as_u64()) {
            return v;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -------- billing header --------

    #[test]
    fn billing_strips_leading_header_with_blank_line() {
        let input = "x-anthropic-billing-header: cc_version=2.1; cch=abc;\n\nYou are helpful.";
        assert_eq!(
            strip_leading_anthropic_billing_header(input),
            "You are helpful."
        );
    }

    #[test]
    fn billing_strips_leading_header_crlf() {
        let input = "x-anthropic-billing-header: abc\r\n\r\nBody";
        assert_eq!(strip_leading_anthropic_billing_header(input), "Body");
    }

    #[test]
    fn billing_keeps_non_leading_header() {
        // 中间或后续出现的同前缀文本不视为 billing header，避免误删用户内容
        let input = "Keep this:\nx-anthropic-billing-header: example";
        assert_eq!(strip_leading_anthropic_billing_header(input), input);
    }

    #[test]
    fn billing_passes_through_non_billing_text() {
        let input = "Just a system prompt";
        assert_eq!(strip_leading_anthropic_billing_header(input), input);
    }

    #[test]
    fn billing_handles_only_header_line() {
        let input = "x-anthropic-billing-header: abc";
        assert_eq!(strip_leading_anthropic_billing_header(input), "");
    }

    #[test]
    fn billing_preserves_user_text_after_header_in_same_block() {
        let input = "x-anthropic-billing-header: abc\n\nStable system";
        assert_eq!(
            strip_leading_anthropic_billing_header(input),
            "Stable system"
        );
    }

    // -------- clean_schema --------

    #[test]
    fn schema_adds_type_and_properties_to_empty_root() {
        let schema = json!({});
        let result = clean_schema(schema);
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn schema_adds_properties_to_typed_root() {
        let schema = json!({"type": "object"});
        let result = clean_schema(schema);
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn schema_preserves_existing_fields() {
        let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}});
        let result = clean_schema(schema.clone());
        assert_eq!(result, schema);
    }

    #[test]
    fn schema_removes_uri_format_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "url": {"type": "string", "format": "uri"}
            }
        });
        let result = clean_schema(schema);
        assert!(result["properties"]["url"].get("format").is_none());
    }

    #[test]
    fn schema_does_not_force_type_on_nested() {
        let schema = json!({
            "type": "object",
            "properties": {
                "nullable_value": {"anyOf": [{"type": "string"}, {"type": "null"}]}
            }
        });
        let result = clean_schema(schema.clone());
        assert_eq!(result, schema);
    }

    #[test]
    fn schema_handles_array_items_recursively() {
        let schema = json!({
            "type": "object",
            "properties": {
                "list": {
                    "type": "array",
                    "items": {"type": "string", "format": "uri"}
                }
            }
        });
        let result = clean_schema(schema);
        assert!(
            result["properties"]["list"]["items"]
                .get("format")
                .is_none()
        );
    }

    // -------- strip_cache_control --------

    #[test]
    fn cache_control_stripped_from_message_content() {
        let mut value = json!({
            "role": "user",
            "content": [{"type": "text", "text": "Hi", "cache_control": {"type": "ephemeral"}}]
        });
        strip_cache_control(&mut value);
        let content_str = serde_json::to_string(&value["content"]).unwrap();
        assert!(!content_str.contains("cache_control"));
    }

    #[test]
    fn cache_control_stripped_recursively_from_system() {
        let mut value = json!({
            "system": [
                {"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}},
                {"type": "text", "text": "sys2"}
            ],
            "tools": [{
                "name": "t",
                "cache_control": {"type": "ephemeral"}
            }]
        });
        strip_cache_control(&mut value);
        let serialized = serde_json::to_string(&value).unwrap();
        assert!(!serialized.contains("cache_control"));
    }

    #[test]
    fn cache_control_preserves_other_fields() {
        let mut value = json!({
            "type": "text",
            "text": "content",
            "cache_control": {"type": "ephemeral"},
            "other_field": "preserved"
        });
        strip_cache_control(&mut value);
        assert_eq!(value["other_field"], "preserved");
        assert!(value.get("cache_control").is_none());
    }

    #[test]
    fn cache_control_no_op_on_value_without_field() {
        let mut value = json!({"type": "text", "text": "clean"});
        let before = value.clone();
        strip_cache_control(&mut value);
        assert_eq!(value, before);
    }

    // -------- extract_cache_field --------

    #[test]
    fn cache_field_prefers_anthropic_top_level() {
        let usage = json!({
            "cache_read_input_tokens": 100,
            "prompt_tokens_details": {"cached_tokens": 50}
        });
        // 顶层优先
        let v = extract_cache_field(
            &usage,
            &["cache_read_input_tokens"],
            &[("prompt_tokens_details", "cached_tokens")],
        );
        assert_eq!(v, 100);
    }

    #[test]
    fn cache_field_falls_back_to_openai_nested() {
        let usage = json!({
            "prompt_tokens_details": {"cached_tokens": 50}
        });
        let v = extract_cache_field(
            &usage,
            &["cache_read_input_tokens"],
            &[("prompt_tokens_details", "cached_tokens")],
        );
        assert_eq!(v, 50);
    }

    #[test]
    fn cache_field_returns_zero_when_absent() {
        let usage = json!({"prompt_tokens": 100});
        let v = extract_cache_field(
            &usage,
            &["cache_read_input_tokens"],
            &[("prompt_tokens_details", "cached_tokens")],
        );
        assert_eq!(v, 0);
    }

    #[test]
    fn cache_field_handles_multiple_openai_aliases() {
        // OpenAI cache_creation_input_tokens 有多种命名
        let usage = json!({
            "prompt_tokens_details": {"cache_write_tokens": 30}
        });
        let v = extract_cache_field(
            &usage,
            &["cache_creation_input_tokens"],
            &[
                ("prompt_tokens_details", "cache_write_tokens"),
                ("prompt_tokens_details", "cache_creation_tokens"),
                ("input_tokens_details", "cache_write_tokens"),
            ],
        );
        assert_eq!(v, 30);
    }
}
