//! 统一内部表示（Internal Representation）
//!
//! 设计目标：为协议转换层提供中立 IR，避免 N 协议 N² 方向的转换函数爆炸。
//! 加新协议只需新增 N 个 Converter（IR ↔ 协议），而非 N² 个方向。
//!
//! 模块布局：
//! - `mod`（本文件）— 类型定义（`InternalRequest`/`InternalResponse`/`ChunkEvent` 等）
//! - `codec` — 非流式 req/resp 的 6 个 parse/emit 函数（Chat/Anthropic/Responses ↔ IR）
//! - `relay` — 流式 SSE chunk 的 parse/emit + 状态机驱动的 stream converter
//!
//! 设计参考：
//! - cc-switch 的 reasoning_bridge envelope 思路（envelope 字段透传不透明上下文）
//! - sub2api 的 `json.RawMessage` 折中（强类型主干 + 透传未知字段）
//! - Anthropic / OpenAI / Responses 三协议字段并集

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashMap;

// 子模块：req/resp 与 IR 的双向映射
pub mod codec;
// 子模块：流式 chunk 与 IR 的双向映射 + stream converter
pub mod relay;

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
    #[allow(dead_code)]
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
    pub reasoning_signature: Option<String>,
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

// ===================== 流式 ChunkEvent（统一中间表示） =====================

/// 流式事件 IR：承载三协议 SSE 事件的并集语义。
///
/// parser（`stream_codec::parse_xxx_chunk`）把上游协议 SSE 解析为 `Vec<ChunkEvent>`，
/// emitter（`stream_codec::emit_xxx_chunk`）持有状态机，消费 `ChunkEvent` 并产生下游 SSE。
#[derive(Debug, Clone)]
pub enum ChunkEvent {
    /// 流开始（Anthropic message_start / Responses response.created / Chat 首帧）
    Start {
        id: String,
        model: String,
        role_announced: bool,
        #[allow(dead_code)]
        usage: Option<InternalUsage>,
    },
    /// 流结束（[DONE]）
    Done,
    /// 内容块开始（仅 Anthropic/Responses 显式，Chat 不区分）
    #[allow(dead_code)]
    BlockStart { index: usize, kind: BlockKind },
    /// 内容块结束
    #[allow(dead_code)]
    BlockStop { index: usize },
    /// 文本增量
    TextDelta(String),
    /// 推理内容增量
    ReasoningDelta(String),
    /// 推理签名增量（Anthropic signature_delta / Chat reasoning_signature）
    ReasoningSignatureDelta(String),
    /// 工具调用开始（id+name 同时到达，或 Chat 端的 content_block_start(tool_use)）
    ToolCallStart {
        upstream_index: usize,
        id: String,
        name: String,
    },
    /// 工具调用参数增量
    ToolCallArgsDelta {
        upstream_index: usize,
        args: String,
    },
    /// 流收尾：finish_reason + usage（Anthropic message_delta / Responses response.completed /
    /// Chat finish_reason 帧）
    Finish {
        reason: InternalFinishReason,
        usage: Option<InternalUsage>,
    },
}

/// 内容块类型（用于 BlockStart 事件）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Text,
    Thinking,
    RedactedThinking,
    ToolUse,
    /// 服务端工具调用（web_search / code_interpreter 等），附类型名
    ServerTool(String),
    /// 服务端工具结果块（流式中不可读，跳过）
    ServerToolResult(String),
    /// 服务端 fallback 块
    Fallback,
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
                content: vec![InternalContent::Text { text: "Hi".into() }],
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
        let mut req = InternalRequest::default();
        req.envelopes
            .insert("anthropic_thinking".into(), "envelope_data".into());
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("envelopes"));
        assert!(!json.contains("anthropic_thinking"));
    }

    #[test]
    fn ir_chunk_event_variants_constructible() {
        let e = ChunkEvent::TextDelta("hi".into());
        assert!(matches!(e, ChunkEvent::TextDelta(_)));
        let e = ChunkEvent::BlockStart {
            index: 0,
            kind: BlockKind::Text,
        };
        assert!(matches!(e, ChunkEvent::BlockStart { .. }));
    }
}
