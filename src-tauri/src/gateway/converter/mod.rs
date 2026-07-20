//! Chat / Responses / Anthropic 三协议间的非流式协议转换。
//!
//! 模块布局：
//! - `helpers` — 通用 helper（ID/时间戳、content 文本、reason 映射、tool_choice 映射）
//! - `request` — 4 个方向的请求体转换
//! - `response` — 4 个方向的响应体转换
//!
//! 本模块（`mod.rs`）仅负责分发：根据 `(src, dst)` 选择对应的转换函数，
//! Responses ↔ Anthropic 双跳通过 Chat 中转。

mod helpers;
mod request;
mod response;

use serde_json::Value;

use crate::config::types::Protocol;
use crate::infra::error::AppError;

// 公开的 helper（外部 `stream` 模块也会用）
pub use helpers::{map_finish_reason_chat_to_anthropic, map_stop_reason_anthropic_to_chat};
// 仅供 `gateway` 内兄弟模块使用（保留原 `pub(super)` 语义）
pub(super) use helpers::{created_now, rand_id};

// 公开的 4 个请求/响应转换函数
pub use request::{anthropic_to_chat_req, chat_to_anthropic_req, chat_to_responses_req, responses_to_chat_req};
pub use response::{
    anthropic_to_chat_resp, chat_to_anthropic_resp, chat_to_responses_resp, responses_to_chat_resp,
};

// ===================== 分发 =====================

pub fn req_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    match (s, d) {
        _ if s == d => Ok(src.clone()),
        (Protocol::Chat, Protocol::Responses) => Ok(chat_to_responses_req(src)),
        (Protocol::Responses, Protocol::Chat) => responses_to_chat_req(src),
        (Protocol::Chat, Protocol::Anthropic) => Ok(chat_to_anthropic_req(src)),
        (Protocol::Anthropic, Protocol::Chat) => Ok(anthropic_to_chat_req(src)),
        (Protocol::Responses, Protocol::Anthropic) => {
            let chat = responses_to_chat_req(src)?;
            Ok(chat_to_anthropic_req(&chat))
        }
        (Protocol::Anthropic, Protocol::Responses) => {
            let chat = anthropic_to_chat_req(src);
            Ok(chat_to_responses_req(&chat))
        }
        _ => Ok(src.clone()),
    }
}

pub fn resp_convert(src: &Value, s: Protocol, d: Protocol) -> Result<Value, AppError> {
    match (s, d) {
        _ if s == d => Ok(src.clone()),
        (Protocol::Responses, Protocol::Chat) => Ok(responses_to_chat_resp(src)),
        (Protocol::Chat, Protocol::Responses) => Ok(chat_to_responses_resp(src)),
        (Protocol::Anthropic, Protocol::Chat) => Ok(anthropic_to_chat_resp(src)),
        (Protocol::Chat, Protocol::Anthropic) => Ok(chat_to_anthropic_resp(src)),
        (Protocol::Anthropic, Protocol::Responses) => {
            let chat = anthropic_to_chat_resp(src);
            Ok(chat_to_responses_resp(&chat))
        }
        (Protocol::Responses, Protocol::Anthropic) => {
            let chat = responses_to_chat_resp(src);
            Ok(chat_to_anthropic_resp(&chat))
        }
        _ => Ok(src.clone()),
    }
}
