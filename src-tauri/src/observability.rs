//! 网关可观测性：Prometheus 风格 metrics + tracing spans 集成。
//!
//! 设计原则：
//! - **零运行时开销**（缺省状态）：使用 `AtomicU64` 计数，无锁
//! - **可配置**：metrics 默认启用，可通过 AppCtrl 关闭
//! - **非侵入**：仅暴露 `GatewayMetrics` struct 和 `record_*` 方法，
//!   不修改现有错误处理路径
//!
//! 暴露的 counters：
//! - `requests_total`：总请求数（按协议分组）
//! - `conversions_total` / `conversions_failed_total`：协议转换成功/失败
//! - `streaming_active` / `streaming_total`：流式请求活跃数和总数
//! - `upstream_errors_total`：上游错误（429/5xx 等）
//! - `first_output_timeouts_total` / `interval_timeouts_total`：流式超时
//! - `tool_call_out_of_order_detected_total`：tool_call 乱序兜底触发次数
//! - `infinite_whitespace_aborted_total`：Copilot 无限空白 bug 中止次数
//!
//! 通过新 IPC 命令 `gateway_metrics` 暴露给前端。
//
// 部分 record_* 方法当前未在所有调用点使用（预留供后续 stream pipeline 集成）。
#![allow(dead_code)]

use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 网关运行时 metrics，使用 AtomicU64 计数器（无锁）
#[derive(Debug, Default)]
pub struct GatewayMetrics {
    /// 总请求数
    pub requests_total: AtomicU64,
    /// Chat 协议请求数
    pub requests_chat: AtomicU64,
    /// Responses 协议请求数
    pub requests_responses: AtomicU64,
    /// Anthropic 协议请求数
    pub requests_anthropic: AtomicU64,
    /// 协议转换成功总数
    pub conversions_total: AtomicU64,
    /// 协议转换失败总数
    pub conversions_failed_total: AtomicU64,
    /// 当前活跃流式请求数（AtomicU64 模拟 i64 计数）
    pub streaming_active: AtomicU64,
    /// 流式请求总数
    pub streaming_total: AtomicU64,
    /// 上游 4xx 错误（非 429）总数
    pub upstream_4xx_total: AtomicU64,
    /// 上游 429 限流总数
    pub upstream_429_total: AtomicU64,
    /// 上游 5xx 错误总数
    pub upstream_5xx_total: AtomicU64,
    /// 上游网络错误总数
    pub upstream_network_error_total: AtomicU64,
    /// 流式首字超时总数
    pub first_output_timeouts_total: AtomicU64,
    /// 流式数据间隔超时总数
    pub interval_timeouts_total: AtomicU64,
    /// tool_call 乱序兜底触发次数（DeepSeek/GLM 场景）
    pub tool_call_out_of_order_total: AtomicU64,
    /// Copilot 无限空白 bug 中止次数
    pub infinite_whitespace_aborted_total: AtomicU64,
    /// billing header 剥离次数（Claude Code 客户端）
    pub billing_headers_stripped_total: AtomicU64,
    /// cache_control 剥离次数（跨协议方向）
    pub cache_control_stripped_total: AtomicU64,
    /// JSON Schema normalize 次数
    pub schema_normalized_total: AtomicU64,
    /// reasoning envelope 编码次数（跨协议 thinking 保真）
    pub reasoning_envelope_encoded_total: AtomicU64,
    /// reasoning envelope 解码次数
    pub reasoning_envelope_decoded_total: AtomicU64,
    /// 累计协议转换耗时（微秒）
    pub conversion_duration_micros_total: AtomicU64,
}

impl GatewayMetrics {
    /// 创建共享的 Arc<GatewayMetrics>
    pub fn shared() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 增加请求计数（按协议）
    pub fn record_request(&self, protocol: crate::config::types::Protocol) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        match protocol {
            crate::config::types::Protocol::Chat => {
                self.requests_chat.fetch_add(1, Ordering::Relaxed);
            }
            crate::config::types::Protocol::Responses => {
                self.requests_responses.fetch_add(1, Ordering::Relaxed);
            }
            crate::config::types::Protocol::Anthropic => {
                self.requests_anthropic.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 记录协议转换成功
    pub fn record_conversion(&self, duration_micros: u64) {
        self.conversions_total.fetch_add(1, Ordering::Relaxed);
        self.conversion_duration_micros_total
            .fetch_add(duration_micros, Ordering::Relaxed);
    }

    /// 记录协议转换失败
    pub fn record_conversion_failed(&self) {
        self.conversions_failed_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 流式请求开始（活跃数 +1，总数 +1）
    pub fn record_streaming_start(&self) {
        self.streaming_active.fetch_add(1, Ordering::Relaxed);
        self.streaming_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 流式请求结束（活跃数 -1）
    pub fn record_streaming_end(&self) {
        self.streaming_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// 记录上游 HTTP 错误
    pub fn record_upstream_error(&self, status: u16) {
        if status == 429 {
            self.upstream_429_total.fetch_add(1, Ordering::Relaxed);
        } else if (400..500).contains(&status) {
            self.upstream_4xx_total.fetch_add(1, Ordering::Relaxed);
        } else if (500..600).contains(&status) {
            self.upstream_5xx_total.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录上游网络错误（连接失败、超时等）
    pub fn record_upstream_network_error(&self) {
        self.upstream_network_error_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录流式首字超时
    pub fn record_first_output_timeout(&self) {
        self.first_output_timeouts_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录流式数据间隔超时
    pub fn record_interval_timeout(&self) {
        self.interval_timeouts_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 tool_call 乱序兜底触发
    pub fn record_tool_call_out_of_order(&self) {
        self.tool_call_out_of_order_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 Copilot 无限空白 bug 中止
    pub fn record_infinite_whitespace_aborted(&self) {
        self.infinite_whitespace_aborted_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 billing header 剥离
    pub fn record_billing_header_stripped(&self) {
        self.billing_headers_stripped_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 cache_control 剥离
    pub fn record_cache_control_stripped(&self) {
        self.cache_control_stripped_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 JSON Schema normalize
    pub fn record_schema_normalized(&self) {
        self.schema_normalized_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 reasoning envelope 编码
    pub fn record_reasoning_envelope_encoded(&self) {
        self.reasoning_envelope_encoded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 记录 reasoning envelope 解码
    pub fn record_reasoning_envelope_decoded(&self) {
        self.reasoning_envelope_decoded_total
            .fetch_add(1, Ordering::Relaxed);
    }

    /// 生成 metrics 快照（供 IPC 命令使用）
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            requests_by_protocol: ProtocolCounts {
                chat: self.requests_chat.load(Ordering::Relaxed),
                responses: self.requests_responses.load(Ordering::Relaxed),
                anthropic: self.requests_anthropic.load(Ordering::Relaxed),
            },
            conversions_total: self.conversions_total.load(Ordering::Relaxed),
            conversions_failed_total: self.conversions_failed_total.load(Ordering::Relaxed),
            streaming_active: self.streaming_active.load(Ordering::Relaxed),
            streaming_total: self.streaming_total.load(Ordering::Relaxed),
            upstream_errors: UpstreamErrorCounts {
                e4xx: self.upstream_4xx_total.load(Ordering::Relaxed),
                e429: self.upstream_429_total.load(Ordering::Relaxed),
                e5xx: self.upstream_5xx_total.load(Ordering::Relaxed),
                network: self.upstream_network_error_total.load(Ordering::Relaxed),
            },
            first_output_timeouts_total: self
                .first_output_timeouts_total
                .load(Ordering::Relaxed),
            interval_timeouts_total: self.interval_timeouts_total.load(Ordering::Relaxed),
            tool_call_out_of_order_total: self
                .tool_call_out_of_order_total
                .load(Ordering::Relaxed),
            infinite_whitespace_aborted_total: self
                .infinite_whitespace_aborted_total
                .load(Ordering::Relaxed),
            billing_headers_stripped_total: self
                .billing_headers_stripped_total
                .load(Ordering::Relaxed),
            cache_control_stripped_total: self
                .cache_control_stripped_total
                .load(Ordering::Relaxed),
            schema_normalized_total: self.schema_normalized_total.load(Ordering::Relaxed),
            reasoning_envelope_encoded_total: self
                .reasoning_envelope_encoded_total
                .load(Ordering::Relaxed),
            reasoning_envelope_decoded_total: self
                .reasoning_envelope_decoded_total
                .load(Ordering::Relaxed),
            conversion_duration_micros_total: self
                .conversion_duration_micros_total
                .load(Ordering::Relaxed),
        }
    }
}

/// 按协议分组的请求计数
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolCounts {
    pub chat: u64,
    pub responses: u64,
    pub anthropic: u64,
}

/// 按状态码分组的上游错误计数
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamErrorCounts {
    pub e4xx: u64,
    pub e429: u64,
    pub e5xx: u64,
    pub network: u64,
}

/// Metrics 快照（IPC 返回类型）
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    pub requests_total: u64,
    pub requests_by_protocol: ProtocolCounts,
    pub conversions_total: u64,
    pub conversions_failed_total: u64,
    pub streaming_active: u64,
    pub streaming_total: u64,
    pub upstream_errors: UpstreamErrorCounts,
    pub first_output_timeouts_total: u64,
    pub interval_timeouts_total: u64,
    pub tool_call_out_of_order_total: u64,
    pub infinite_whitespace_aborted_total: u64,
    pub billing_headers_stripped_total: u64,
    pub cache_control_stripped_total: u64,
    pub schema_normalized_total: u64,
    pub reasoning_envelope_encoded_total: u64,
    pub reasoning_envelope_decoded_total: u64,
    pub conversion_duration_micros_total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_zero_for_default() {
        let m = GatewayMetrics::default();
        let s = m.snapshot();
        assert_eq!(s.requests_total, 0);
        assert_eq!(s.conversions_total, 0);
        assert_eq!(s.streaming_active, 0);
    }

    #[test]
    fn record_request_increments_correct_protocol() {
        use crate::config::types::Protocol;
        let m = GatewayMetrics::default();
        m.record_request(Protocol::Chat);
        m.record_request(Protocol::Chat);
        m.record_request(Protocol::Anthropic);
        let s = m.snapshot();
        assert_eq!(s.requests_total, 3);
        assert_eq!(s.requests_by_protocol.chat, 2);
        assert_eq!(s.requests_by_protocol.anthropic, 1);
        assert_eq!(s.requests_by_protocol.responses, 0);
    }

    #[test]
    fn streaming_active_decrements_on_end() {
        let m = GatewayMetrics::default();
        m.record_streaming_start();
        m.record_streaming_start();
        assert_eq!(m.streaming_active.load(Ordering::Relaxed), 2);
        m.record_streaming_end();
        assert_eq!(m.streaming_active.load(Ordering::Relaxed), 1);
        assert_eq!(m.streaming_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn upstream_error_classified_by_status() {
        let m = GatewayMetrics::default();
        m.record_upstream_error(400);
        m.record_upstream_error(429);
        m.record_upstream_error(429);
        m.record_upstream_error(500);
        let s = m.snapshot();
        assert_eq!(s.upstream_errors.e4xx, 1);
        assert_eq!(s.upstream_errors.e429, 2);
        assert_eq!(s.upstream_errors.e5xx, 1);
    }
}
