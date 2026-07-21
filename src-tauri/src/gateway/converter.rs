//! Chat / Responses / Anthropic 三协议间的非流式协议转换。
//!
//! 基于 IR（Internal Representation）的统一中间表示实现：
//! - `req_convert(src, s, d)`：A 协议 JSON → IR → B 协议 JSON
//! - `resp_convert(src, s, d)`：同上，针对响应体
//!
//! 真正的 parse/emit 实现在 `crate::gateway::ir::codec` 中。
//!
//! 通过 IR 中转，N 协议只需 N 个 parse + N 个 emit 函数（共 2N），而非 N² 个方向。
//! Responses ↔ Anthropic 不再走 Chat 双跳，直接经 IR 单跳。

use serde_json::Value;

use crate::config::types::Protocol;
use crate::error::AppError;
use crate::gateway::ir::codec::{
    emit_anthropic_req, emit_anthropic_resp, emit_chat_req, emit_chat_resp, emit_responses_req,
    emit_responses_resp, parse_anthropic_req, parse_anthropic_resp, parse_chat_req,
    parse_chat_resp, parse_responses_req, parse_responses_resp,
};

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

// ===================== 分发（A → IR → B） =====================

/// 请求体转换：根据 `(src, dst)` 选择 parse/emit 函数。
///
/// 所有方向（含 Responses ↔ Anthropic）都走 IR 单跳，消除原双跳经 Chat 的字段丢失风险。
pub fn req_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    if s == d {
        return Ok(src.clone());
    }
    // A → IR
    let ir = match s {
        Protocol::Chat => parse_chat_req(src)?,
        Protocol::Anthropic => parse_anthropic_req(src)?,
        Protocol::Responses => parse_responses_req(src)?,
    };
    // IR → B
    let out = match d {
        Protocol::Chat => emit_chat_req(&ir),
        Protocol::Anthropic => emit_anthropic_req(&ir),
        Protocol::Responses => emit_responses_req(&ir),
    };
    Ok(out)
}

/// 响应体转换：同 req_convert，针对响应体。
pub fn resp_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    if s == d {
        return Ok(src.clone());
    }
    // A → IR
    let ir = match s {
        Protocol::Chat => parse_chat_resp(src),
        Protocol::Anthropic => parse_anthropic_resp(src),
        Protocol::Responses => parse_responses_resp(src),
    };
    // IR → B
    let out = match d {
        Protocol::Chat => emit_chat_resp(&ir),
        Protocol::Anthropic => emit_anthropic_resp(&ir),
        Protocol::Responses => emit_responses_resp(&ir),
    };
    Ok(out)
}
