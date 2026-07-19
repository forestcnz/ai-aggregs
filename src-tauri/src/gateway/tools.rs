//! 高级工具生态支持（namespace / custom / tool_search）
//!
//! 实现 Codex CLI 客户端使用的高级工具语义在跨协议转换时的处理：
//! - **namespace 工具**：摊平为 `<ns>__<name>` 形式发给 Chat/Anthropic 上游
//! - **custom/freeform 工具**：降级为带 `input: string` 单参数的 function 工具
//! - **tool_search 工具**：代理为同名 function 工具
//!
//! 借鉴 sub2api `chatcompletions_responses_bridge.go` 和 cc-switch
//! `transform_codex_anthropic.rs` 的设计，但适配 ai-aggregs 的 Value 操作风格。
//
// 部分函数当前未被 converter.rs 调用（预留用于响应方向还原）。
#![allow(dead_code)]

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

/// 收集 Responses 请求中 namespace 子工具的摊平名 → 原始归属映射。
///
/// 响应方向（如 Chat → Responses）需要据此把模型对摊平名的调用还原为带
/// `namespace` 字段的 function_call item（Codex 按 namespace+name 路由）。
pub fn build_namespace_owner_map(tools: &[Value]) -> std::collections::HashMap<String, NamespaceOwner> {
    let mut out = std::collections::HashMap::new();
    for tool in tools {
        let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if tool_type != "namespace" {
            continue;
        }
        let ns_name = tool.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if ns_name.is_empty() {
            continue;
        }
        // Responses namespace 用 tools 或 children 字段（语义相同）
        let children: Option<&Vec<Value>> = tool
            .get("tools")
            .and_then(|t| t.as_array())
            .or_else(|| tool.get("children").and_then(|c| c.as_array()));
        let Some(children) = children else {
            continue;
        };
        for (flat_tool, owner) in flatten_namespace_children(ns_name, children) {
            let flat_name = flat_tool
                .pointer("/function/name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if !flat_name.is_empty() {
                out.insert(flat_name, owner);
            }
        }
    }
    out
}

/// 收集 Responses 请求中 custom/freeform 工具的名字集合。
///
/// Chat 桥回程时需要据此把模型对这些工具的调用还原为 custom_tool_call 项
/// （Codex 只按该类型路由）。
pub fn collect_custom_tool_names(tools: &[Value]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for tool in tools {
        let tool_type = tool.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if tool_type == "custom" || tool_type == "freeform" {
            if let Some(name) = tool.get("name").and_then(|n| n.as_str()) {
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    }
    out
}

/// 检测 Responses 请求是否声明了 tool_search 服务端工具。
pub fn has_tool_search_tool(tools: &[Value]) -> bool {
    tools
        .iter()
        .any(|t| t.get("type").and_then(|v| v.as_str()) == Some("tool_search"))
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

/// 从降级 function 调用的 arguments JSON 中还原 custom 工具的自由文本输入。
///
/// 优先取 `{"input": "..."}` 的 input 字段；模型未按 schema 输出时原样回传，
/// 交由客户端校验、模型重试。
pub fn extract_custom_tool_input(arguments: &str) -> String {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
        return trimmed.to_string();
    };
    if let Some(input) = obj.get("input").and_then(|v| v.as_str()) {
        return input.to_string();
    }
    // 模型未按 schema 输出，原样回传
    trimmed.to_string()
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
    fn build_namespace_owner_map_basic() {
        let tools = vec![json!({
            "type": "namespace",
            "name": "mcp",
            "tools": [{"type": "function", "name": "search"}]
        })];
        let map = build_namespace_owner_map(&tools);
        assert_eq!(map.len(), 1);
        let owner = map.get("mcp__search").unwrap();
        assert_eq!(owner.namespace, "mcp");
        assert_eq!(owner.name, "search");
    }

    #[test]
    fn build_namespace_owner_map_uses_children_field() {
        let tools = vec![json!({
            "type": "namespace",
            "name": "ns",
            "children": [{"type": "function", "name": "child"}]
        })];
        let map = build_namespace_owner_map(&tools);
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("ns__child"));
    }

    #[test]
    fn collect_custom_tool_names_basic() {
        let tools = vec![
            json!({"type": "function", "name": "f1"}),
            json!({"type": "custom", "name": "apply_patch"}),
            json!({"type": "freeform", "name": "exec"}),
        ];
        let names = collect_custom_tool_names(&tools);
        assert_eq!(names.len(), 2);
        assert!(names.contains("apply_patch"));
        assert!(names.contains("exec"));
    }

    #[test]
    fn has_tool_search_detects_proxy() {
        let tools = vec![
            json!({"type": "function", "name": "f1"}),
            json!({"type": "tool_search"}),
        ];
        assert!(has_tool_search_tool(&tools));

        let tools_without = vec![json!({"type": "function", "name": "f1"})];
        assert!(!has_tool_search_tool(&tools_without));
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

    #[test]
    fn extract_custom_tool_input_from_input_field() {
        let args = r#"{"input": "patch content here"}"#;
        let extracted = extract_custom_tool_input(args);
        assert_eq!(extracted, "patch content here");
    }

    #[test]
    fn extract_custom_tool_input_falls_back_to_raw() {
        let args = r#"{"foo": "bar"}"#;
        let extracted = extract_custom_tool_input(args);
        // 没有 input 字段，原样回传
        assert_eq!(extracted, r#"{"foo": "bar"}"#);
    }

    #[test]
    fn extract_custom_tool_input_handles_empty() {
        assert_eq!(extract_custom_tool_input(""), "");
        assert_eq!(extract_custom_tool_input("   "), "");
    }

    #[test]
    fn extract_custom_tool_input_handles_invalid_json() {
        let extracted = extract_custom_tool_input("not json");
        assert_eq!(extracted, "not json");
    }
}
