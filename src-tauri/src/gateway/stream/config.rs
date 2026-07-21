//! 流式管线配置（超时与心跳），以及 SSE chunk 安全拼接工具。
//!
//! `StreamConfig` 由 `ProviderConfig` 的可选字段构造（provider 级覆盖），缺省回退全局默认。
//! `append_utf8_safe` 处理跨 chunk 的 UTF-8 多字节字符边界。

use std::time::Duration;

use bytes::BytesMut;

/// 流式请求的超时与心跳配置。
///
/// 所有字段为 `Option<Duration>`：`None` 表示禁用对应特性。
/// 默认值通过 `StreamConfig::default()` 提供（keepalive=15s / first-output=120s）。
///
/// 配置来源（优先级递减）：
/// 1. ProviderConfig 的 `stream_*` 字段（provider 级覆盖）
/// 2. 本模块的全局默认常量
#[derive(Debug, Clone)]
pub struct StreamConfig {
    /// 心跳间隔：周期性向下游发 SSE 注释 `: keepalive\n\n`，防反代空闲断开
    pub keepalive_interval: Option<Duration>,
    /// 首字超时：上游首个有效 chunk 到达前的最长等待
    pub first_output_timeout: Option<Duration>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            keepalive_interval: Some(Duration::from_secs(15)),
            first_output_timeout: Some(Duration::from_secs(120)),
        }
    }
}

/// SSE 心跳行（注释格式，不影响 SSE 事件解析）
pub(super) const KEEPALIVE_LINE: &str = ": keepalive\n\n";

/// 安全地将 chunk 追加到 buf，处理跨 chunk 的 UTF-8 多字节字符边界。
///
/// 如果 chunk 的尾部是不完整的 UTF-8 序列（多字节字符被 TCP chunk 切断），
/// 将不完整部分暂存到 `remainder`，下次调用时拼接。
/// 避免 `from_utf8_lossy` 产生 U+FFFD 替换字符。
///
/// 借鉴 cc-switch `proxy/sse.rs::append_utf8_safe` 的设计。
pub(super) fn append_utf8_safe(buf: &mut BytesMut, remainder: &mut Vec<u8>, chunk: &[u8]) {
    // 拼接上次的不完整尾部
    let mut combined: Vec<u8> = std::mem::take(remainder);
    combined.extend_from_slice(chunk);

    // 尝试将整块作为 UTF-8 解析
    match std::str::from_utf8(&combined) {
        Ok(_) => {
            // 全部合法
            buf.extend_from_slice(&combined);
        }
        Err(e) => {
            let safe_len = e.valid_up_to();
            buf.extend_from_slice(&combined[..safe_len]);
            // 剩余部分存入 remainder
            // e.error_len() == None 表示不完整序列（尾部被切断），下次拼接即可
            // e.error_len() == Some(_) 表示真正的非法字节，也会被暂存（下次仍会失败，但不丢数据）
            remainder.extend_from_slice(&combined[safe_len..]);
        }
    }
}
