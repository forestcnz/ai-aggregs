//! 流式/非流式请求的 token 用量统计。
//!
//! - `UsageCtx` 在请求生命周期内累积用量，结束时通过 `spawn_blocking` 异步写入 DB。
//! - `extract_usage` 兼容 Chat / Anthropic / Responses 三种 usage 格式，并合并 cache token。
//! - `sniff_usage` 从单条 SSE payload 中嗅探并更新最新用量。

use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::gateway::convert_helpers::extract_cache_field;
use crate::infra::db;

/// 流式/非流式请求的用量记录上下文
pub struct UsageCtx {
    pub consumer_key: String,
    pub model: String,
    pub provider_id: i64,
    pub provider_key: String,
    pub db: Arc<Mutex<rusqlite::Connection>>,
}

impl UsageCtx {
    /// 记录 token 用量。在 spawn_blocking 中执行同步 DB 操作，
    /// 避免阻塞 tokio runtime 的 async worker thread（用量表行数大时聚合写入可能耗时）。
    pub fn record(&self, input: u64, output: u64, total: u64) {
        if input == 0 && output == 0 {
            return;
        }
        let consumer_key = self.consumer_key.clone();
        let model = self.model.clone();
        let provider_id = self.provider_id;
        let provider_key = self.provider_key.clone();
        let db = self.db.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Ok(conn) = db.lock() {
                let _ = db::record_usage(&conn, &consumer_key, &model, input, output, total);
                let _ = db::record_provider_usage(
                    &conn,
                    provider_id,
                    &provider_key,
                    &model,
                    input,
                    output,
                    total,
                );
            }
        });
    }
}

/// 从任意 JSON 值中提取 token 用量（兼容 Chat / Anthropic / Responses 三种格式）。
/// 返回 (input, output, total)。Anthropic 的 cache_creation/cache_read 也计入 input，
/// 与上游计费规则一致（prompt cache 写入/读取的 token 同样按输入价计费）。
pub fn extract_usage(v: &Value) -> Option<(u64, u64, u64)> {
    // Chat 风格：usage.prompt_tokens / completion_tokens / total_tokens
    if let Some(u) = v.get("usage") {
        if let Some(pt) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
            let ct = u
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            // 兼容 OpenAI 多种 cache token 字段命名：
            // - 顶层（Anthropic 标准 / Kimi 等）：cache_read_input_tokens / cache_creation_input_tokens
            // - nested（OpenAI 标准 / 别名）：prompt_tokens_details.{cached,cache_write,cache_creation}_tokens
            //   + input_tokens_details.cache_write_tokens
            // 顶层优先于 nested（兼容性最高）
            let cached = extract_cache_field(
                u,
                &["cache_read_input_tokens"],
                &[("prompt_tokens_details", "cached_tokens")],
            );
            let cache_creation = extract_cache_field(
                u,
                &["cache_creation_input_tokens"],
                &[
                    ("prompt_tokens_details", "cache_write_tokens"),
                    ("prompt_tokens_details", "cache_creation_tokens"),
                    ("input_tokens_details", "cache_write_tokens"),
                ],
            );
            // 加法哲学：cache 部分（命中+写入）同样按输入价计费，与上游计费规则一致
            let prompt_total = pt + cached + cache_creation;
            let tt = u
                .get("total_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(prompt_total + ct);
            return Some((prompt_total, ct, tt));
        }
        // Anthropic / Responses 风格：usage.input_tokens / output_tokens
        // 加上 cache_creation_input_tokens / cache_read_input_tokens
        if let Some(it) = u.get("input_tokens").and_then(|x| x.as_u64()) {
            let cache_creation = u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let cache_read = u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let total_input = it + cache_creation + cache_read;
            return Some((total_input, ot, total_input + ot));
        }
    }
    // Responses completed 事件：response.usage.input_tokens
    if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
        if let Some(it) = u.get("input_tokens").and_then(|x| x.as_u64()) {
            let cache_creation = u
                .get("cache_creation_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let cache_read = u
                .get("cache_read_input_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let ot = u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0);
            let total_input = it + cache_creation + cache_read;
            return Some((total_input, ot, total_input + ot));
        }
    }
    None
}

/// 从单个 SSE payload 字符串中嗅探用量，更新 last_usage
pub(super) fn sniff_usage(payload: &str, last_usage: &mut Option<(u64, u64, u64)>) {
    if payload == "[DONE]" {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(payload) {
        if let Some(u) = extract_usage(&v) {
            *last_usage = Some(u);
        }
    }
}
