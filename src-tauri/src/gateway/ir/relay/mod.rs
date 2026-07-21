//! 流式 SSE chunk 与 IR 的双向映射 + 4 个方向 stream converter。
//!
//! 设计：
//! - parser（`parse_xxx_event`）：上游协议 SSE event/data → `Vec<ChunkEvent>`，无状态纯函数
//! - emitter（`XxxEmitter`）：`ChunkEvent` -> 下游协议 SSE 字符串，**持有状态机**
//!   （AnthropicEmitter 的 ensure_text/thinking/tool_use、Copilot 无限空白检测、
//!   late starts 兜底；ResponsesEmitter 的 reasoning/message item 生命周期等）
//! - `IrStreamConverter`：组合 (parser, emitter)，对外暴露 `StreamConverter` 接口
//!
//! 跨协议双跳（如 Anthropic→Responses）直接走 IR，不再经 Chat 中转。

pub mod chat;
pub mod anthropic;
pub mod responses;

use crate::config::types::Protocol;
use crate::gateway::ir::ChunkEvent;
use crate::gateway::stream::StreamConverter;

// Copilot 无限空白 bug 阈值（与原 chat_to_anthropic.rs 保持一致）
const INFINITE_WHITESPACE_THRESHOLD: usize = 500;



// ===================== IrStreamConverter：parser + emitter 组合 =====================

enum AnyEmitter {
    Chat(Box<chat::ChatEmitter>),
    Anthropic(Box<anthropic::AnthropicEmitter>),
    Responses(Box<responses::ResponsesEmitter>),
}

/// 通用 IR 流转换器：根据 src 协议选 parser，根据 dst 协议选 emitter。
///
/// 替代原 4 个独立 stream converter + Chained 组合器。
/// 跨协议双跳（如 Anthropic→Responses）直接走 IR，不再经 Chat 中转。
pub struct IrStreamConverter {
    src: Protocol,
    emitter: AnyEmitter,
}

impl IrStreamConverter {
    pub fn new(src: Protocol, dst: Protocol) -> Self {
        let emitter = match dst {
            Protocol::Chat => AnyEmitter::Chat(Box::new(chat::ChatEmitter::new())),
            Protocol::Anthropic => AnyEmitter::Anthropic(Box::new(anthropic::AnthropicEmitter::new())),
            Protocol::Responses => AnyEmitter::Responses(Box::new(responses::ResponsesEmitter::new())),
        };
        Self { src, emitter }
    }

    fn parse(&self, event: Option<&str>, data: &str) -> Vec<ChunkEvent> {
        match self.src {
            Protocol::Chat => chat::parse_chat_event(event, data),
            Protocol::Anthropic => anthropic::parse_anthropic_event(event, data),
            Protocol::Responses => responses::parse_responses_event(event, data),
        }
    }
}

impl StreamConverter for IrStreamConverter {
    fn on_event(&mut self, event: Option<&str>, data: &str) -> Vec<String> {
        let events = self.parse(event, data);
        let mut out = Vec::new();
        for ev in events {
            let payloads = match &mut self.emitter {
                AnyEmitter::Chat(e) => e.on_event(ev),
                AnyEmitter::Anthropic(e) => e.on_event(ev),
                AnyEmitter::Responses(e) => e.on_event(ev),
            };
            out.extend(payloads);
        }
        out
    }

    fn on_done(&mut self) -> Vec<String> {
        match &mut self.emitter {
            AnyEmitter::Chat(e) => e.on_done(),
            AnyEmitter::Anthropic(e) => e.on_done(),
            AnyEmitter::Responses(e) => e.on_done(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::types::Protocol;
    use crate::gateway::stream::StreamConverter;

    /// 验证：上游 Responses 以 encrypted 模式发 reasoning（output_item.added/done 含
    /// encrypted_content，无 summary_text delta）时，转换到 Anthropic 客户端能正确产生
    /// content_block_start(thinking) + signature_delta + content_block_stop，
    /// 保证多轮 thinking 完整性。
    ///
    /// 这是 OpenAI 标准 Responses 协议定义的两种 reasoning 输出形态之一。
    #[test]
    fn responses_to_anthropic_reasoning_item_block_translates_to_thinking_block() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Anthropic);

        // 1) response.created
        let created = r#"{"type":"response.created","response":{"id":"resp_1","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        // 2) response.output_item.added (type=reasoning, summary=[], encrypted_content="blob_x")
        let added = r#"{"type":"response.output_item.added","output_index":0,"sequence_number":2,"item":{"id":"rs_1","type":"reasoning","content":[],"encrypted_content":"blob_x","summary":[]}}"#;
        let out_added = conv.on_event(None, added);
        let combined_added: String = out_added.iter().cloned().collect();
        // 应当开启 thinking 块（content_block_start + type=thinking）
        assert!(
            combined_added.contains("content_block_start") && combined_added.contains("\"thinking\""),
            "reasoning item added should open thinking block, got: {combined_added}"
        );

        // 3) response.output_item.done (type=reasoning) —— 关闭 thinking 块，
        //    关闭前应 flush encrypted_content 作为 signature_delta
        let done = r#"{"type":"response.output_item.done","output_index":0,"sequence_number":3,"item":{"id":"rs_1","type":"reasoning","content":[],"encrypted_content":"blob_x","summary":[]}}"#;
        let out_done = conv.on_event(None, done);
        let combined_done: String = out_done.iter().cloned().collect();
        assert!(
            combined_done.contains("signature_delta") && combined_done.contains("blob_x"),
            "thinking block close should flush encrypted_content as signature_delta, got: {combined_done}"
        );
        assert!(
            combined_done.contains("content_block_stop"),
            "reasoning item done should close thinking block, got: {combined_done}"
        );
    }

    /// 验证：当 reasoning item 的 summary 含明文 summary_text 时，转换到 Anthropic
    /// 会作为 thinking_delta 输出（而非仅作为加密块）。
    #[test]
    fn responses_to_anthropic_reasoning_with_summary_text_emits_thinking_delta() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Anthropic);

        let created = r#"{"type":"response.created","response":{"id":"resp_2","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_2","type":"reasoning","summary":[{"type":"summary_text","text":"I should answer."}],"encrypted_content":"sig_abc"}}"#;
        let out = conv.on_event(None, added);
        let combined: String = out.iter().cloned().collect();
        assert!(
            combined.contains("thinking_delta") && combined.contains("I should answer."),
            "summary_text should be emitted as thinking_delta, got: {combined}"
        );
    }

    /// 验证：转换到 Chat 客户端时，reasoning item 的 encrypted_content 作为
    /// reasoning_signature 在 Finish 帧透传（Chat 协议无"块"概念，依赖 Finish 累积）。
    #[test]
    fn responses_to_chat_reasoning_item_signature_passed_in_finish_frame() {
        let mut conv = IrStreamConverter::new(Protocol::Responses, Protocol::Chat);

        let created = r#"{"type":"response.created","response":{"id":"resp_3","object":"response","status":"in_progress","output":[]}}"#;
        let _ = conv.on_event(None, created);

        let added = r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"rs_3","type":"reasoning","summary":[],"encrypted_content":"blob_y"}}"#;
        let _ = conv.on_event(None, added);

        let done = r#"{"type":"response.output_item.done","output_index":0,"item":{"id":"rs_3","type":"reasoning","summary":[],"encrypted_content":"blob_y"}}"#;
        let _ = conv.on_event(None, done);

        // response.completed 触发 Finish + Done
        let completed = r#"{"type":"response.completed","response":{"id":"resp_3","object":"response","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hi"}]}],"usage":{"input_tokens":10,"output_tokens":5}}}"#;
        let out = conv.on_event(None, completed);
        let combined: String = out.iter().cloned().collect();
        assert!(
            combined.contains("reasoning_signature") && combined.contains("blob_y"),
            "encrypted_content should be flushed as reasoning_signature in finish frame, got: {combined}"
        );
    }
}
