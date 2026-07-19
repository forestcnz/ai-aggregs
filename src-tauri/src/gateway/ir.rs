//! 统一内部表示（Internal Representation）
//!
//! 设计目标：为协议转换层提供中立 IR，避免 N 协议 N² 方向的转换函数爆炸。
//! 加新协议只需新增 N 个 Converter（IR ↔ 协议），而非 N² 个方向。
//!
//! 当前状态：**类型定义预留**。现有 converter.rs 仍是直接 A→B 转换；
//! 后续 PR 将迁移到 IR-based 实现。本模块仅定义类型，无运行时开销。
//!
//! 设计参考：
//! - cc-switch 的 reasoning_bridge envelope 思路（envelope 字段透传不透明上下文）
//! - sub2api 的 `json.RawMessage` 折中（强类型主干 + 透传未知字段）
//! - Anthropic / OpenAI / Responses 三协议字段并集

// 预留基础设施：类型定义已完整，将在后续 PR 中集成到转换管线。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

/// 统一内部请求：作为协议转换的中间表示。
///
/// 设计原则：
/// 1. **强类型主干**：核心字段（model、messages、tools 等）显式定义
/// 2. **extensions 透传**：未知字段通过 `extensions` Map 透传，避免漏字段
/// 3. **envelope 携带**：跨协议保真数据（reasoning signature 等）通过 envelopes 传递
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InternalRequest {
    /// 模型 ID（所有协议共有）
    pub model: String,
    /// 系统提示（合并后的单一字符串）
    #[serde(default)]
    pub system: Option<String>,
    /// 对话消息列表
    #[serde(default)]
    pub messages: Vec<InternalMessage>,
    /// 工具定义列表
    #[serde(default)]
    pub tools: Vec<InternalTool>,
    /// 工具选择策略
    #[serde(default)]
    pub tool_choice: Option<InternalToolChoice>,
    /// 最大 token 数
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// 采样温度
    #[serde(default)]
    pub temperature: Option<f32>,
    /// Top-P 采样
    #[serde(default)]
    pub top_p: Option<f32>,
    /// 停止序列
    #[serde(default)]
    pub stop: Vec<String>,
    /// 是否流式
    #[serde(default)]
    pub stream: bool,
    /// 推理控制
    #[serde(default)]
    pub reasoning: Option<InternalReasoning>,
    /// 并行工具调用（仅 Chat/Responses 支持）
    #[serde(default)]
    pub parallel_tool_calls: Option<bool>,
    /// 透传未知字段（借鉴 Go json.RawMessage）
    ///
    /// 协议特有字段（如 Anthropic metadata、Responses previous_response_id、
    /// Chat service_tier 等）放在此处，转换时直接拷贝到目标协议。
    #[serde(default, flatten)]
    pub extensions: Map<String, Value>,
    /// 跨协议 envelope 数据（reasoning_bridge 写入）
    ///
    /// key 约定：
    /// - `anthropic_thinking`：原 Anthropic thinking 块（base64 envelope）
    /// - `openai_reasoning`：原 Responses reasoning item（base64 envelope）
    #[serde(skip)]
    pub envelopes: HashMap<String, String>,
}

/// 统一消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalMessage {
    pub role: InternalRole,
    #[serde(default)]
    pub content: Vec<InternalContent>,
    #[serde(default)]
    pub tool_calls: Vec<InternalToolCall>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub reasoning: Option<InternalReasoningBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InternalRole {
    System,
    User,
    Assistant,
    Tool,
}

/// 统一内容块（多模态）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InternalContent {
    Text { text: String },
    Image {
        url: String,
        media_type: String,
        #[serde(default)]
        data: Option<String>,
    },
    Audio {
        data: String,
        media_type: String,
    },
    File {
        url: String,
        filename: String,
    },
}

/// 统一工具定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema（已规范化）
    pub parameters: Value,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub kind: InternalToolKind,
}

/// 工具类型（支持 namespace/custom/tool_search 等高级语义）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InternalToolKind {
    /// 普通 function 工具
    #[default]
    Function,
    /// Codex custom/freeform 工具（自由文本输入）
    Custom {
        /// custom 工具降级为 function 时的 input schema
        input_schema: Value,
    },
    /// Codex namespace 工具（含子工具）
    Namespace {
        children: Vec<InternalTool>,
    },
    /// 服务端工具（如 web_search）
    ServerTool {
        platform: String,
        /// 服务端工具具体类型（如 "web_search"）。
        /// 重命名避免与 serde 内部 tag `kind` 冲突。
        tool_kind: String,
    },
    /// Codex tool_search 代理工具
    ToolSearch {
        proxy_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalToolCall {
    pub id: String,
    pub name: String,
    /// arguments 字符串（JSON 编码）
    pub arguments: String,
    /// namespace 工具的归属命名空间（Codex 特有）
    #[serde(default)]
    pub namespace: Option<String>,
    /// custom 工具的自由文本输入（非 JSON）
    #[serde(default)]
    pub custom_input: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InternalToolChoice {
    /// 简单选择：auto / none / required
    Simple(String),
    /// 具名工具
    Named {
        name: String,
        #[serde(default)]
        namespace: Option<String>,
    },
}

/// 推理控制
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InternalReasoning {
    /// low / medium / high / xhigh / max
    #[serde(default)]
    pub effort: Option<String>,
    /// Anthropic thinking budget
    #[serde(default)]
    pub budget_tokens: Option<u32>,
    /// Responses reasoning summary
    #[serde(default)]
    pub summary: Option<String>,
    /// Responses encrypted_content（不透明加密 blob）
    #[serde(default)]
    pub encrypted_content: Option<String>,
    /// Anthropic thinking signature
    #[serde(default)]
    pub signature: Option<String>,
}

/// 推理历史块（assistant message 中的历史 thinking）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalReasoningBlock {
    pub thinking: String,
    #[serde(default)]
    pub signature: Option<String>,
    #[serde(default)]
    pub redacted: bool,
}

// ===================== InternalResponse =====================

/// 统一内部响应
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InternalResponse {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub content: Vec<InternalContent>,
    #[serde(default)]
    pub tool_calls: Vec<InternalToolCall>,
    #[serde(default)]
    pub reasoning: Option<InternalReasoningBlock>,
    #[serde(default)]
    pub finish_reason: InternalFinishReason,
    #[serde(default)]
    pub usage: InternalUsage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InternalUsage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InternalFinishReason {
    #[default]
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

/// 流式 chunk（单个 SSE event 的 IR 表示）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalChunk {
    #[serde(default)]
    pub delta_content: Option<String>,
    #[serde(default)]
    pub delta_reasoning: Option<String>,
    #[serde(default)]
    pub delta_tool_calls: Vec<InternalToolCallDelta>,
    #[serde(default)]
    pub finish_reason: Option<InternalFinishReason>,
    #[serde(default)]
    pub usage: Option<InternalUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InternalToolCallDelta {
    pub index: usize,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ir_request_serializes_basic_fields() {
        let req = InternalRequest {
            model: "gpt-5".into(),
            system: Some("You are helpful".into()),
            messages: vec![InternalMessage {
                role: InternalRole::User,
                content: vec![InternalContent::Text {
                    text: "Hi".into(),
                }],
                tool_calls: vec![],
                tool_call_id: None,
                reasoning: None,
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"model\":\"gpt-5\""));
        assert!(json.contains("You are helpful"));
    }

    #[test]
    fn ir_extensions_preserves_unknown_fields() {
        let mut req = InternalRequest::default();
        req.extensions
            .insert("previous_response_id".into(), serde_json::json!("resp_1"));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("previous_response_id"));
        // 反序列化回来
        let back: InternalRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.extensions.get("previous_response_id"),
            Some(&serde_json::json!("resp_1"))
        );
    }

    #[test]
    fn ir_tool_kind_function_default() {
        let tool = InternalTool {
            name: "weather".into(),
            description: None,
            parameters: serde_json::json!({}),
            strict: false,
            kind: InternalToolKind::Function,
        };
        assert!(matches!(tool.kind, InternalToolKind::Function));
    }

    #[test]
    fn ir_tool_kind_namespace_carries_children() {
        let tool = InternalTool {
            name: "mcp".into(),
            description: None,
            parameters: serde_json::json!({}),
            strict: false,
            kind: InternalToolKind::Namespace {
                children: vec![InternalTool {
                    name: "search".into(),
                    description: None,
                    parameters: serde_json::json!({}),
                    strict: false,
                    kind: InternalToolKind::Function,
                }],
            },
        };
        if let InternalToolKind::Namespace { children } = &tool.kind {
            assert_eq!(children.len(), 1);
            assert_eq!(children[0].name, "search");
        } else {
            panic!("should be Namespace");
        }
    }

    #[test]
    fn ir_envelopes_not_serialized() {
        // envelopes 字段标记 #[serde(skip)]，不应出现在序列化中
        let mut req = InternalRequest::default();
        req.envelopes
            .insert("anthropic_thinking".into(), "envelope_data".into());
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("envelopes"));
        assert!(!json.contains("anthropic_thinking"));
    }

    #[test]
    fn ir_internal_chunk_default() {
        let chunk = InternalChunk {
            delta_content: None,
            delta_reasoning: None,
            delta_tool_calls: vec![],
            finish_reason: None,
            usage: None,
        };
        // 验证可序列化
        let _ = serde_json::to_string(&chunk).unwrap();
    }
}
