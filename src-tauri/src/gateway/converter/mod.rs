//! Chat / Responses / Anthropic 三协议间的非流式协议转换。
//!
//! 基于 IR（Internal Representation）的统一中间表示实现：
//! - `req_convert(src, s, d)`：A 协议 JSON → IR → B 协议 JSON
//! - `resp_convert(src, s, d)`：同上，针对响应体
//!
//! 模块布局：
//! - `helpers` — 通用 helper（ID/时间戳、stop_reason 映射、finish_reason 映射；
//!   部分（rand_id/created_now/map_*_reason）被 stream 模块也用）
//! - 真正的 parse/emit 实现在 `crate::gateway::ir::codec` 中
//!
//! 通过 IR 中转，N 协议只需 N 个 parse + N 个 emit 函数（共 2N），而非 N² 个方向。
//! Responses ↔ Anthropic 不再走 Chat 双跳，直接经 IR 单跳。

mod helpers;

use serde_json::Value;

use crate::config::types::Protocol;
use crate::gateway::ir::codec::{
    emit_anthropic_req, emit_anthropic_resp, emit_chat_req, emit_chat_resp, emit_responses_req,
    emit_responses_resp, parse_anthropic_req, parse_anthropic_resp, parse_chat_req,
    parse_chat_resp, parse_responses_req, parse_responses_resp,
};
use crate::infra::error::AppError;

// 公开的 helper（外部 `stream` 模块也会用）
pub use helpers::{
    created_now, map_finish_reason_chat_to_anthropic, rand_id,
};

// 兼容性占位：原 map_stop_reason_anthropic_to_chat 仍被部分场景使用，
// 但 IR 化后主要由 ir/codec.rs 内部处理；这里保留导出以防外部调用。
#[allow(unused_imports)]
pub use helpers::map_stop_reason_anthropic_to_chat;

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
