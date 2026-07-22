//! 高级工具生态支持（namespace / custom / tool_search）
//!
//! 实现 Codex CLI 客户端使用的高级工具语义在跨协议转换时的处理：
//! - **namespace 工具**：摊平为 `<ns>__<name>` 形式发给 Chat/Anthropic 上游
//! - **custom/freeform 工具**：降级为带 `input: string` 单参数的 function 工具
//! - **tool_search 工具**：代理为同名 function 工具
//!
//! 借鉴 sub2api `chatcompletions_responses_bridge.go` 和 cc-switch
//! `transform_codex_anthropic.rs` 的设计，但适配 ai-aggregs 的 Value 操作风格。

use serde_json::{json, Value};

/// Chat 协议 function 工具名长度上限（OpenAI 限制）
pub const CHAT_TOOL_NAME_MAX_LEN: usize = 64;

/// custom/freeform 工具降级为 function 工具时的 input schema
pub const CUSTOM_TOOL_INPUT_SCHEMA: &str = r#"{"type":"object","properties":{"input":{"type":"string","description":"The raw input for this tool, passed through verbatim."}},"required":["input"]}"#;

/// tool_search 代理工具名
pub const TOOL_SEARCH_PROXY_NAME: &str = "tool_search";

/// tool_search 代理工具 schema
pub const TOOL_SEARCH_PROXY_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Search query for tools or connectors to load."},"limit":{"type":"integer","description":"Maximum number of tool groups to return."}},"required":["query"]}"#;

/// 把 namespace 工具的子 function 工具摊平为顶层 function 工具。
///
/// 摊平规则：
/// - `<namespace>__<name>` 形式（双下划线分隔）
/// - 超长（>64 字符）截断 + sha256 短哈希后缀
///
/// 返回值：摊平后的工具 + 摊平名 → (namespace, name) 映射（用于响应方向还原）
///
/// # Panics
/// 不会 panic（sha256 计算和编码内部都用 unwrap_or 兜底）。
pub fn flatten_namespace_children(
    namespace: &str,
    children: &[Value],
) -> Vec<(Value, NamespaceOwner)> {
    if namespace.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for child in children {
        let child_type = child
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("function");
        if child_type != "function" {
            continue;
        }
        let child_name = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if child_name.is_empty() {
            continue;
        }
        let flat = flatten_namespace_tool_name(namespace, child_name);
        let owner = NamespaceOwner {
            namespace: namespace.to_string(),
            name: child_name.to_string(),
        };
        // 构造摊平后的 function 工具定义
        let flat_tool = json!({
            "type": "function",
            "function": {
                "name": flat,
                "description": child.get("description").cloned().unwrap_or(Value::Null),
                "parameters": child.get("parameters").cloned().unwrap_or(json!({})),
            }
        });
        out.push((flat_tool, owner));
    }
    out
}

/// 生成 namespace 子工具的摊平名；超长截断 + sha256 短哈希保证唯一性。
///
/// 借鉴 sub2api `chatcompletions_responses_bridge.go:747` 的 `flattenNamespaceToolName`。
pub fn flatten_namespace_tool_name(namespace: &str, name: &str) -> String {
    let full = format!("{namespace}__{name}");
    if full.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full;
    }
    // 超长：截断 + sha256 短哈希后缀（4 字节 = 8 hex 字符）
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(full.as_bytes());
    let hash = hasher.finalize();
    let suffix = format!("__{}", hex::encode(&hash[..4]));
    let prefix_len = CHAT_TOOL_NAME_MAX_LEN - suffix.len();
    // 按 char boundary 截断（避免切到多字节字符中间）
    let prefix: String = full.chars().take(prefix_len).collect();
    format!("{prefix}{suffix}")
}

/// namespace 摊平名 → 原始归属映射条目
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceOwner {
    pub namespace: String,
    pub name: String,
}

/// 生成 tool_search 代理工具（function 形式）。
pub fn tool_search_proxy_function() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL_SEARCH_PROXY_NAME,
            "description": "Search and load Codex tools, plugins, connectors, and MCP namespaces for the current task.",
            "parameters": json!(TOOL_SEARCH_PROXY_SCHEMA),
        }
    })
}

/// 生成 custom/freeform 工具降级后的 function 形式。
pub fn custom_tool_to_function(tool: &Value) -> Value {
    let name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "parameters": json!(CUSTOM_TOOL_INPUT_SCHEMA),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flatten_simple_namespace() {
        let flat = flatten_namespace_tool_name("mcp", "search");
        assert_eq!(flat, "mcp__search");
    }

    #[test]
    fn flatten_long_namespace_truncates_with_hash() {
        // 构造超长 namespace + name
        let long_ns = "x".repeat(40);
        let long_name = "y".repeat(40);
        let flat = flatten_namespace_tool_name(&long_ns, &long_name);
        assert!(flat.len() <= CHAT_TOOL_NAME_MAX_LEN);
        // 应包含 sha256 后缀格式："__" + 8 hex 字符
        // 找最后一个 "__" 分隔符
        let last_underscore = flat.rfind("__").unwrap();
        let suffix_hex = &flat[last_underscore + 2..];
        assert_eq!(suffix_hex.len(), 8, "suffix should be 8 hex chars");
        assert!(
            suffix_hex.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix should be hex"
        );
    }

    #[test]
    fn flatten_namespace_children_basic() {
        let children = vec![json!({
            "type": "function",
            "name": "search",
            "description": "search tool",
            "parameters": {"type": "object"}
        })];
        let result = flatten_namespace_children("mcp", &children);
        assert_eq!(result.len(), 1);
        let (flat_tool, owner) = &result[0];
        assert_eq!(
            flat_tool.pointer("/function/name").and_then(|n| n.as_str()),
            Some("mcp__search")
        );
        assert_eq!(owner.namespace, "mcp");
        assert_eq!(owner.name, "search");
    }

    #[test]
    fn flatten_skips_non_function_children() {
        let children = vec![
            json!({"type": "function", "name": "f1"}),
            json!({"type": "web_search"}), // 非 function
            json!({"type": "function", "name": "f2"}),
        ];
        let result = flatten_namespace_children("ns", &children);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn tool_search_proxy_function_shape() {
        let f = tool_search_proxy_function();
        assert_eq!(
            f.pointer("/function/name").and_then(|n| n.as_str()),
            Some("tool_search")
        );
        // parameters 应是 string 形式的 JSON schema（OpenAI Responses 形式）
        let params = f
            .pointer("/function/parameters")
            .and_then(|p| p.as_str())
            .unwrap_or("");
        assert!(params.contains("query"));
        assert!(params.contains("limit"));
    }

    #[test]
    fn custom_tool_to_function_uses_input_schema() {
        let custom = json!({
            "type": "custom",
            "name": "apply_patch",
            "description": "Apply file patch"
        });
        let f = custom_tool_to_function(&custom);
        assert_eq!(
            f.pointer("/function/name").and_then(|n| n.as_str()),
            Some("apply_patch")
        );
        let params = f
            .pointer("/function/parameters")
            .and_then(|p| p.as_str())
            .unwrap_or("");
        assert!(params.contains("input"));
        assert!(params.contains("string"));
    }
}
