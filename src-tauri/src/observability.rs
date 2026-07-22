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
//! - `upstream_errors_total`：上游错误（429/5xx 等）
//!
//! 通过 IPC 命令 `gateway_metrics` 暴露给前端。

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
    /// 上游 4xx 错误（非 429）总数
    pub upstream_4xx_total: AtomicU64,
    /// 上游 429 限流总数
    pub upstream_429_total: AtomicU64,
    /// 上游 5xx 错误总数
    pub upstream_5xx_total: AtomicU64,
    /// 上游网络错误总数
    pub upstream_network_error_total: AtomicU64,
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
            upstream_errors: UpstreamErrorCounts {
                e4xx: self.upstream_4xx_total.load(Ordering::Relaxed),
                e429: self.upstream_429_total.load(Ordering::Relaxed),
                e5xx: self.upstream_5xx_total.load(Ordering::Relaxed),
                network: self.upstream_network_error_total.load(Ordering::Relaxed),
            },
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
    pub upstream_errors: UpstreamErrorCounts,
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
