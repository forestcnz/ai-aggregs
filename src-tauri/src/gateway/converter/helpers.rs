//! 协议转换的通用 helper：ID/时间戳生成、stop_reason / finish_reason 映射。
//!
//! 这些函数被 `crate::gateway::ir` 模块和 `crate::gateway::stream` 模块共用，
//! 通过 `super::mod.rs` 的 `pub use` 暴露给外部。
//!
//! 注：原本的 content 文本提取、tool_choice 映射等已迁移到 `ir/codec.rs`（IR-based 实现）。

use serde_json::Value;

// ===================== ID / 时间戳 =====================

/// 生成十六进制纳秒时间戳 ID（无外部依赖，适合临时 ID）
pub fn rand_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{n:x}")
}

/// 当前 Unix 时间戳（秒）
pub fn created_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===================== stop_reason / finish_reason 映射 =====================

/// Anthropic stop_reason -> Chat finish_reason
///
/// 注：IR 化后大部分场景由 `ir/codec.rs` 内部映射处理；
/// 但 `stream` 模块的 `AnthropicEmitter` 仍需此映射（在 emit Chat 流时复用）。
#[allow(dead_code)]
pub fn map_stop_reason_anthropic_to_chat(sr: &str) -> String {
    match sr {
        "end_turn" | "stop_sequence" | "pause_turn" => "stop".into(),
        "tool_use" => "tool_calls".into(),
        "max_tokens" | "model_context_window_exceeded" => "length".into(),
        "refusal" | "unsafe_content" => "content_filter".into(),
        _ => sr.into(),
    }
}

/// Chat finish_reason → Anthropic stop_reason
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

// 兼容性占位：原 helpers.rs 中有大量 pub(super) 函数（chat_content_to_text 等），
// 已迁移到 ir/codec.rs。这里保留一个 Value 的导入以便未来扩展。
#[allow(dead_code)]
fn _ensure_value_import_used(_v: &Value) {}
