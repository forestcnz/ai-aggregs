//! 跨协议 reasoning envelope：让 thinking signature 经 Chat 中转时不丢失
//!
//! ## 背景
//!
//! ai-aggregs 以 Chat 作为事实 IR，Anthropic ↔ Responses 互转时经 Chat 双跳：
//! ```text
//! Anthropic 请求 → anthropic_to_chat_req → Chat 请求 → chat_to_responses_req → Responses 请求
//! ```
//!
//! 问题：Anthropic `thinking.signature` 在 Chat 协议中没有标准字段。早期实现把它
//! 塞在非标准扩展 `reasoning_signature`，但 `chat_to_responses_req` 不识别该字段，
//! 导致跨协议多轮对话时 reasoning 上下文断链。
//!
//! ## 方案
//!
//! 把"对方协议的完整 reasoning 上下文"序列化为 base64 envelope，加版本化前缀，
//! 作为 Chat `reasoning_signature` 字段值透传。下游协议再解码还原。
//!
//! envelope 仅在跨协议方向生效；同协议透传时不触发。旧版客户端发送的非 envelope
//! 字符串通过 `is_envelope` 判定后回退到原逻辑（透传不解码），保证向后兼容。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::Value;

/// Anthropic thinking 块的 envelope 前缀。
///
/// 语义：当前协议是 Anthropic（thinking 块含 signature），目标是经 Chat 中转
/// 后还原为 Anthropic signature（双向场景）。
pub(crate) const ANTHROPIC_THINKING_ENVELOPE_PREFIX: &str = "aiaggregs-anthropic-thinking-v1:";

/// OpenAI Responses reasoning item 的 envelope 前缀。
///
/// 语义：当前协议是 Responses（reasoning item 含 encrypted_content），目标是经
/// Chat 中转后还原为 Responses reasoning（双向场景）。
pub(crate) const OPENAI_REASONING_ENVELOPE_PREFIX: &str = "aiaggregs-openai-reasoning-v1:";

/// 把 Anthropic thinking 块（含 signature）编码为 envelope 字符串。
///
/// 仅当 `block` 是 thinking 类型且 `signature` 字段非空时编码；
/// 无 signature 的 thinking 块返回 None（不需要跨协议保真）。
///
/// 防嵌套：若 signature 本身已是 envelope（异常场景），原样返回不二次编码。
pub(crate) fn encode_anthropic_thinking(block: &Value) -> Option<String> {
    if block.get("type").and_then(|v| v.as_str()) != Some("thinking") {
        return None;
    }
    let sig = block
        .get("signature")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if sig.is_empty() {
        return None;
    }
    // 防嵌套编码（极端场景：上层多次调用）
    if is_envelope(sig) {
        return Some(sig.to_string());
    }
    let bytes = serde_json::to_vec(block).ok()?;
    Some(format!(
        "{ANTHROPIC_THINKING_ENVELOPE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// 把 OpenAI Responses reasoning item 编码为 envelope 字符串。
///
/// reasoning item 必须含 `encrypted_content`（OpenAI 的不透明加密 blob）才有
/// 跨协议保真的价值；仅含 summary 文本的 reasoning 退化到原 summary 逻辑。
pub(crate) fn encode_openai_reasoning(item: &Value) -> Option<String> {
    if item.get("type").and_then(|v| v.as_str()) != Some("reasoning") {
        return None;
    }
    let has_encrypted = item
        .get("encrypted_content")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    if !has_encrypted {
        return None;
    }
    let bytes = serde_json::to_vec(item).ok()?;
    Some(format!(
        "{OPENAI_REASONING_ENVELOPE_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes)
    ))
}

/// 解码 envelope 字符串，自动识别两种前缀。
///
/// 非 envelope 输入返回 None（调用方应回退到原透传逻辑）。
pub(crate) fn decode_envelope(envelope: &str) -> Option<Value> {
    let payload = envelope
        .strip_prefix(ANTHROPIC_THINKING_ENVELOPE_PREFIX)
        .or_else(|| envelope.strip_prefix(OPENAI_REASONING_ENVELOPE_PREFIX))?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

/// 判断字符串是否为 envelope（任一前缀匹配）。
///
/// 用于在 `chat_to_responses_req` / `chat_to_anthropic_req` 等下游转换中
/// 区分 envelope（需解码）和普通 signature 字符串（直接透传）。
pub(crate) fn is_envelope(s: &str) -> bool {
    s.starts_with(ANTHROPIC_THINKING_ENVELOPE_PREFIX)
        || s.starts_with(OPENAI_REASONING_ENVELOPE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_thinking_with_signature_round_trips() {
        let block = json!({
            "type": "thinking",
            "thinking": "Need a tool",
            "signature": "sig_abc123"
        });
        let envelope = encode_anthropic_thinking(&block).unwrap();
        assert!(envelope.starts_with(ANTHROPIC_THINKING_ENVELOPE_PREFIX));
        let decoded = decode_envelope(&envelope).unwrap();
        assert_eq!(decoded, block);
    }

    #[test]
    fn openai_reasoning_with_encrypted_content_round_trips() {
        let item = json!({
            "type": "reasoning",
            "id": "rs_1",
            "summary": [{"type": "summary_text", "text": "Need a tool"}],
            "encrypted_content": "opaque-blob"
        });
        let envelope = encode_openai_reasoning(&item).unwrap();
        assert!(envelope.starts_with(OPENAI_REASONING_ENVELOPE_PREFIX));
        let decoded = decode_envelope(&envelope).unwrap();
        assert_eq!(decoded, item);
    }

    #[test]
    fn signature_less_thinking_not_encoded() {
        let block = json!({"type": "thinking", "thinking": "no signature"});
        assert!(encode_anthropic_thinking(&block).is_none());
    }

    #[test]
    fn summary_only_reasoning_not_encoded() {
        let item = json!({
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "no encrypted"}]
        });
        assert!(encode_openai_reasoning(&item).is_none());
    }

    #[test]
    fn non_thinking_block_not_encoded() {
        let block = json!({"type": "text", "text": "hello"});
        assert!(encode_anthropic_thinking(&block).is_none());
    }

    #[test]
    fn non_envelope_string_not_decoded() {
        // 旧版客户端发送的普通 signature 字符串应回退到原逻辑
        assert!(decode_envelope("sig_abc123").is_none());
        assert!(decode_envelope("").is_none());
        assert!(decode_envelope("random text").is_none());
    }

    #[test]
    fn is_envelope_detects_both_prefixes() {
        let anthropic_env = "aiaggregs-anthropic-thinking-v1:abc";
        let openai_env = "aiaggregs-openai-reasoning-v1:def";
        let plain = "sig_abc123";
        assert!(is_envelope(anthropic_env));
        assert!(is_envelope(openai_env));
        assert!(!is_envelope(plain));
    }

    #[test]
    fn nested_encoding_idempotent() {
        let block = json!({
            "type": "thinking",
            "thinking": "x",
            "signature": "sig"
        });
        let env1 = encode_anthropic_thinking(&block).unwrap();
        // 把 envelope 作为 signature 再次编码（模拟异常调用），应原样返回
        let fake_block = json!({
            "type": "thinking",
            "thinking": "x",
            "signature": env1
        });
        let env2 = encode_anthropic_thinking(&fake_block).unwrap();
        assert_eq!(env1, env2);
    }

    #[test]
    fn corrupted_envelope_returns_none() {
        assert!(decode_envelope("aiaggregs-anthropic-thinking-v1:!!!invalid-base64!!!").is_none());
        assert!(decode_envelope("aiaggregs-anthropic-thinking-v1:").is_none());
    }
}
