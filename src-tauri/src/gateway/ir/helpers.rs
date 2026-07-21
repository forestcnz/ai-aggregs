//! IR 模块公用 helper：ID/时间戳生成、finish_reason 映射。
//!
//! 这些函数原本在 `converter/helpers.rs` 中定义，但因被 `ir/codec.rs` 和 `ir/stream_codec.rs`
//! 使用造成依赖倒挂，故迁移到 `ir/` 模块内。`converter/` 通过 `pub use crate::gateway::ir::helpers::*`
//! 重新导出，保持外部兼容。

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
