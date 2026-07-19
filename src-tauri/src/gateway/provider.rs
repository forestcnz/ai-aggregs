use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;

use crate::config::types::{ApiKeyEntry, Protocol, ProviderConfig};

// ===================== ProtocolAdapter trait =====================

/// 协议适配器：抽象不同协议（Chat/Anthropic/Responses）的协议特化行为。
///
/// 设计动机：避免 `Provider::inject_reasoning` 和 `Provider::auth_headers_for`
/// 内部 `match self.protocol` 分支随新协议增长。新增协议只需新增 trait 实现，
/// 不需要修改 `Provider` struct 本身。
///
/// 当前实现：Chat / Anthropic / Responses 三种协议。
/// 未来可扩展：Gemini、Bedrock 等（不在本版范围）。
#[allow(dead_code)]
pub trait ProtocolAdapter: Send + Sync {
    /// 协议标识
    fn protocol(&self) -> Protocol;
    /// URL endpoint（如 `/chat/completions`、`/messages`）
    fn endpoint(&self) -> &'static str;
    /// 构造鉴权 header 列表（按协议不同：Bearer / x-api-key 等）
    fn auth_headers(&self, key: &str) -> Vec<(HeaderName, HeaderValue)>;
    /// 注入 reasoning effort 参数到请求体
    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str);
    /// 流式请求时注入 stream_options.include_usage（仅 Chat 协议需要）
    fn inject_stream_options(&self, _body: &mut serde_json::Value, _stream: bool) {}
}

/// OpenAI Chat Completions 适配器
pub struct ChatAdapter;

impl ProtocolAdapter for ChatAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Chat
    }
    fn endpoint(&self) -> &'static str {
        "/chat/completions"
    }
    fn auth_headers(&self, key: &str) -> Vec<(HeaderName, HeaderValue)> {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
            vec![(HeaderName::from_static("authorization"), v)]
        } else {
            vec![]
        }
    }
    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str) {
        body["reasoning_effort"] = serde_json::Value::String(effort.to_string());
    }
    fn inject_stream_options(&self, body: &mut serde_json::Value, stream: bool) {
        if stream {
            // 流式 Chat 请求注入 stream_options.include_usage，确保上游在末尾 chunk 返回 token 用量
            let so = body
                .as_object_mut()
                .map(|o| {
                    o.entry("stream_options")
                        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
                })
                .and_then(|v| v.as_object_mut());
            if let Some(opts) = so {
                opts.insert(
                    "include_usage".to_string(),
                    serde_json::Value::Bool(true),
                );
            }
        }
    }
}

/// Anthropic Messages 适配器
pub struct AnthropicAdapter;

impl ProtocolAdapter for AnthropicAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Anthropic
    }
    fn endpoint(&self) -> &'static str {
        "/messages"
    }
    fn auth_headers(&self, key: &str) -> Vec<(HeaderName, HeaderValue)> {
        let mut headers = Vec::new();
        if let Ok(v) = HeaderValue::from_str(key) {
            headers.push((HeaderName::from_static("x-api-key"), v));
        }
        headers.push((
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        ));
        headers
    }
    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str) {
        use serde_json::json;
        if body.get("thinking").is_none() {
            let model = body
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            // Claude 4.6+ / Sonnet 5 / Fable 5 / Mythos 等新模型不支持 manual thinking，
            // 必须用 adaptive；旧模型（Claude 3.x、Opus 4.1/4.5、Haiku 4.5）用 enabled
            let needs_adaptive = model.contains("4-6")
                || model.contains("4-7")
                || model.contains("4-8")
                || model.contains("sonnet-5")
                || model.contains("fable")
                || model.contains("mythos");
            // 根据 effort 映射 budget_tokens（借鉴 cc-switch effort_to_thinking_budget）
            // 仅 enabled 模式下生效；adaptive 模式由模型自主决定 budget
            let budget = match effort.to_ascii_lowercase().as_str() {
                "low" | "minimal" => 2048u32,
                "medium" => 8192,
                "high" => 16384,
                "xhigh" | "max" => 24576,
                _ => 8192,
            };
            body["thinking"] = if needs_adaptive {
                json!({"type": "adaptive"})
            } else {
                json!({"type": "enabled", "budget_tokens": budget})
            };
        }
    }
}

/// OpenAI Responses API 适配器
pub struct ResponsesAdapter;

impl ProtocolAdapter for ResponsesAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::Responses
    }
    fn endpoint(&self) -> &'static str {
        "/responses"
    }
    fn auth_headers(&self, key: &str) -> Vec<(HeaderName, HeaderValue)> {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
            vec![(HeaderName::from_static("authorization"), v)]
        } else {
            vec![]
        }
    }
    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str) {
        body["reasoning"] = serde_json::json!({"effort": effort, "summary": "auto"});
    }
}

/// 根据 protocol 构造对应的 adapter
fn adapter_for(protocol: Protocol) -> Box<dyn ProtocolAdapter> {
    match protocol {
        Protocol::Chat => Box::new(ChatAdapter),
        Protocol::Anthropic => Box::new(AnthropicAdapter),
        Protocol::Responses => Box::new(ResponsesAdapter),
    }
}

// ===================== Provider =====================

#[derive(Debug)]
pub struct UpstreamError {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "upstream {}: {}", self.status, self.message)
    }
}

impl std::error::Error for UpstreamError {}

#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub idx: usize,
    pub masked: String,
    pub enabled: bool,
    pub blacklisted: bool,
    pub blacklist_remaining_secs: Option<u64>,
}

pub struct Provider {
    pub id: i64,
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub models: Vec<String>,
    pub reasoning_effort: Option<String>,
    /// 流式 keepalive 心跳间隔（None 表示用全局默认）
    #[allow(dead_code)]
    pub stream_keepalive_interval_secs: Option<u64>,
    /// 流式首字超时（None 表示用全局默认）
    #[allow(dead_code)]
    pub stream_first_output_timeout_secs: Option<u64>,
    /// 流式数据间隔超时（None 表示用全局默认）
    #[allow(dead_code)]
    pub stream_interval_timeout_secs: Option<u64>,
    /// 是否检测 Copilot 无限空白 bug（None 表示用全局默认 true）
    #[allow(dead_code)]
    pub detect_infinite_whitespace: Option<bool>,
    keys: Vec<ApiKeyEntry>,
    blacklist: Mutex<HashMap<usize, Instant>>,
    blacklist_disabled_until: Mutex<Option<Instant>>,
    blacklist_secs: u64,
    client: reqwest::Client,
    /// 非流式请求的总超时（流式请求不应用此超时，避免长 SSE 被切断）
    timeout: Duration,
    /// 上次成功使用的密钥索引，下次优先尝试
    last_key_idx: Mutex<Option<usize>>,
    /// 协议适配器：抽象 Chat/Anthropic/Responses 的协议特化行为
    adapter: Box<dyn ProtocolAdapter>,
}

impl Provider {
    pub fn new(cfg: &ProviderConfig, blacklist_secs: u64) -> anyhow::Result<Self> {
        // 客户端层面只设置 connect 超时（连接建立阶段）。
        // 总请求超时由 RequestBuilder::timeout() 按是否流式分别设置：
        //   - 非流式：应用 timeout_secs 限制整体响应时间
        //   - 流式：不设置请求超时，避免 SSE 长流被中途切断
        //   （SSE 可能持续数分钟，特别是 reasoning model）
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        let adapter = adapter_for(cfg.protocol);
        Ok(Self {
            id: cfg.id,
            name: cfg.name.clone(),
            protocol: cfg.protocol,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            models: cfg.models.clone(),
            reasoning_effort: cfg.reasoning_effort.clone(),
            stream_keepalive_interval_secs: cfg.stream_keepalive_interval_secs,
            stream_first_output_timeout_secs: cfg.stream_first_output_timeout_secs,
            stream_interval_timeout_secs: cfg.stream_interval_timeout_secs,
            detect_infinite_whitespace: cfg.detect_infinite_whitespace,
            keys: cfg.api_keys.clone(),
            blacklist: Mutex::new(HashMap::new()),
            blacklist_disabled_until: Mutex::new(None),
            blacklist_secs,
            timeout: Duration::from_secs(cfg.timeout_secs),
            client,
            last_key_idx: Mutex::new(None),
            adapter,
        })
    }

    pub fn key_statuses(&self) -> Vec<KeyStatus> {
        let now = Instant::now();
        let bl = self.blacklist.lock().unwrap();
        self.keys
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let enabled = entry.enabled();
                let (blacklisted, remaining) = match bl.get(&idx) {
                    Some(until) if *until > now => {
                        let secs = until.duration_since(now).as_secs();
                        (true, Some(secs))
                    }
                    _ => (false, None),
                };
                KeyStatus {
                    idx,
                    masked: mask_key(entry.key()),
                    enabled,
                    blacklisted,
                    blacklist_remaining_secs: remaining,
                }
            })
            .collect()
    }

    fn is_blacklisted(&self, idx: usize, now: Instant) -> bool {
        let map = self.blacklist.lock().unwrap();
        matches!(map.get(&idx), Some(until) if *until > now)
    }

    fn blacklist_key(&self, idx: usize, now: Instant) {
        let mut map = self.blacklist.lock().unwrap();
        map.insert(idx, now + Duration::from_secs(self.blacklist_secs));
    }

    fn is_blacklist_disabled(&self, now: Instant) -> bool {
        let guard = self.blacklist_disabled_until.lock().unwrap();
        guard.map(|u| now < u).unwrap_or(false)
    }

    fn all_enabled_keys_blacklisted(&self, now: Instant) -> bool {
        let map = self.blacklist.lock().unwrap();
        let enabled_idxs: Vec<usize> = self
            .keys
            .iter()
            .enumerate()
            .filter(|(_, e)| e.enabled())
            .map(|(i, _)| i)
            .collect();
        if enabled_idxs.is_empty() {
            return false;
        }
        enabled_idxs
            .iter()
            .all(|&i| map.get(&i).map(|u| *u > now).unwrap_or(false))
    }

    fn auth_headers_for(&self, key: &str) -> HeaderMap {
        // 委托给 ProtocolAdapter，避免 match self.protocol 分支爆炸
        let mut h = HeaderMap::new();
        for (name, value) in self.adapter.auth_headers(key) {
            h.insert(name, value);
        }
        h
    }

    pub fn endpoint(&self) -> &'static str {
        self.protocol.endpoint()
    }

    /// 从 Provider 的流式配置字段构造 StreamConfig
    pub fn stream_config(&self) -> crate::gateway::stream::StreamConfig {
        crate::gateway::stream::StreamConfig {
            keepalive_interval: self
                .stream_keepalive_interval_secs
                .filter(|&v| v > 0)
                .map(std::time::Duration::from_secs),
            first_output_timeout: self
                .stream_first_output_timeout_secs
                .filter(|&v| v > 0)
                .map(std::time::Duration::from_secs),
            interval_timeout: self
                .stream_interval_timeout_secs
                .filter(|&v| v > 0)
                .map(std::time::Duration::from_secs),
        }
    }

    pub async fn send(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        stream: bool,
        incoming: &HeaderMap,
    ) -> Result<(reqwest::Response, String), UpstreamError> {
        // 请求头策略：Consumer 请求头完整透传，但必须剥离两类头：
        //   1. 认证头（authorization / x-api-key / anthropic-version）— 由 auth_headers_for
        //      按协议注入 provider 自己的 key（key 替换是唯一改写点）。
        //   2. 传输相关头（host / content-length / content-type）— reqwest 必须基于实际
        //      上游 URL 和最终 body 重新计算，否则会触发上游路由失败 / 长度不一致 / 编码错误：
        //        - host：透传 consumer 的 127.0.0.1:8849 会让 nginx 路由失败返回 403
        //        - content-length：body 可能被改写（别名重定向 / stream_options / reasoning_effort）
        //        - content-type：.json() 会自动设为 application/json
        //   accept-encoding 完整透传：reqwest 已启用 gzip/brotli/deflate feature，
        //   会按响应的 content-encoding 自动解压，无需关心上游压缩策略变化。
        let mut forward = incoming.clone();
        for h in [
            "authorization",
            "x-api-key",
            "anthropic-version",
            "host",
            "content-length",
            "content-type",
        ] {
            forward.remove(h);
        }
        let url = format!("{}{}", self.base_url, endpoint);
        let send_body: serde_json::Value = if let Some(eff) = &self.reasoning_effort {
            let mut b = body.clone();
            self.inject_reasoning(&mut b, eff);
            b
        } else {
            body.clone()
        };
        tracing::debug!(provider = %self.name, url = %url, body = %send_body, "→ 上游请求");
        // 有多少个启用的 key 就最多尝试多少个（不再受 max_retries 限制）
        let enabled_count = self.keys.iter().filter(|e| e.enabled()).count();
        let total = enabled_count.max(1);
        let mut last_err: Option<UpstreamError> = None;

        // 构建密钥尝试顺序：上次成功的密钥优先，其余按原序
        let last = *self.last_key_idx.lock().unwrap();
        let mut order: Vec<usize> = (0..self.keys.len()).collect();
        if let Some(li) = last {
            if li < self.keys.len() {
                order.retain(|&i| i != li);
                order.insert(0, li);
            }
        }

        loop {
            let now = Instant::now();
            let disabled = self.is_blacklist_disabled(now);
            let mut tried = 0usize;

            for &idx in &order {
                if tried >= total {
                    break;
                }
                if !self.keys[idx].enabled() {
                    continue;
                }
                let now = Instant::now();
                if !disabled && self.is_blacklisted(idx, now) {
                    continue;
                }

                let key = self.keys[idx].key();
                let masked = mask_key(key);
                // 最终请求头 = provider 认证（按 p_proto）+ Consumer 透传头
                let mut headers = self.auth_headers_for(key);
                for (name, val) in forward.iter() {
                    headers.append(name.clone(), val.clone());
                }
                // debug：输出最终发送给上游的请求头（剔除认证头，避免泄漏密钥）
                {
                    let dbg_headers: Vec<String> = headers
                        .iter()
                        .filter(|(n, _)| {
                            let s = n.as_str();
                            s != "authorization" && s != "x-api-key"
                        })
                        .map(|(n, v)| format!("{}={}", n, v.to_str().unwrap_or("<binary>")))
                        .collect();
                    tracing::debug!(
                        provider = %self.name,
                        idx,
                        key = %masked,
                        headers = ?dbg_headers,
                        "→ 上游请求头（不含认证）"
                    );
                }
                let mut req = self.client.post(&url).headers(headers).json(&send_body);
                if stream {
                    // 流式请求：仅设置 accept 头，不设请求超时（SSE 流可能持续数分钟）
                    req = req.header("accept", "text/event-stream");
                } else {
                    // 非流式请求：应用 provider 配置的总超时
                    req = req.timeout(self.timeout);
                }

                tried += 1;

                match req.send().await {
                    Ok(r) if r.status().is_success() => {
                        *self.last_key_idx.lock().unwrap() = Some(idx);
                        tracing::info!(
                            provider = %self.name,
                            idx,
                            key = %masked,
                            status = %r.status(),
                            "upstream ok"
                        );
                        return Ok((r, key.to_string()));
                    }
                    Ok(r) => {
                        let status = r.status();
                        let code = status.as_u16();
                        let text = r.text().await.unwrap_or_default();
                        if code == 429 {
                            self.blacklist_key(idx, now);
                            tracing::warn!(
                                provider = %self.name, idx, key = %masked,
                                "key 429, blacklisted"
                            );
                            last_err = Some(UpstreamError {
                                status: code,
                                message: text,
                            });
                        } else if status.is_client_error() {
                            tracing::warn!(
                                provider = %self.name, idx, key = %masked, status = code,
                                "upstream client error, no retry"
                            );
                            last_err = Some(UpstreamError {
                                status: code,
                                message: text,
                            });
                            break;
                        } else {
                            tracing::warn!(
                                provider = %self.name, idx, key = %masked, status = code,
                                "upstream server error, retry next key"
                            );
                            last_err = Some(UpstreamError {
                                status: code,
                                message: text,
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            provider = %self.name, idx, key = %masked, err = %e,
                            "send error, retry next key"
                        );
                        last_err = Some(UpstreamError {
                            status: 502,
                            message: e.to_string(),
                        });
                    }
                }
            }

            if !disabled && self.all_enabled_keys_blacklisted(Instant::now()) {
                self.blacklist.lock().unwrap().clear();
                *self.blacklist_disabled_until.lock().unwrap() =
                    Some(Instant::now() + Duration::from_secs(600));
                tracing::warn!(
                    provider = %self.name,
                    "all keys blacklisted, cleared blacklist and disabled for 10 min"
                );
                continue;
            }

            break;
        }
        Err(last_err.unwrap_or_else(|| UpstreamError {
            status: 502,
            message: "no key available".into(),
        }))
    }

    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str) {
        // 委托给 ProtocolAdapter
        self.adapter.inject_reasoning(body, effort);
    }
}

fn mask_key(key: &str) -> String {
    // 全程按字符（Unicode scalar）切分，避免落在 UTF-8 多字节字符中间导致 panic
    // 短 key 也保证首尾都露一部分：
    //   len <= 2  → 首1 + **
    //   len <= 6  → 首1 + ** + 尾1
    //   len <= 12 → 首3 + ** + 尾3
    //   len > 12  → 首6 + ** + 尾6
    let chars: Vec<char> = key.chars().collect();
    let len = chars.len();
    if len <= 2 {
        return format!("{}**", chars[0]);
    }
    let (hl, tl) = if len <= 6 {
        (1, 1)
    } else if len <= 12 {
        (3, 3)
    } else {
        (6, 6)
    };
    let head: String = chars[..hl].iter().collect();
    let tail: String = chars[len - tl..].iter().collect();
    format!("{head}**{tail}")
}

#[cfg(test)]
mod mask_tests {
    use super::mask_key;

    #[test]
    fn very_short_key_shows_head_only() {
        // len <= 2：首1 + **（首尾分离无意义）
        assert_eq!(mask_key("a"), "a**");
        assert_eq!(mask_key("ab"), "a**");
    }

    #[test]
    fn short_key_shows_head_and_tail() {
        // len 3..=6：首1 + ** + 尾1
        assert_eq!(mask_key("abc"), "a**c");
        assert_eq!(mask_key("sk-1"), "s**1");
        assert_eq!(mask_key("abcdef"), "a**f");
    }

    #[test]
    fn medium_key_shows_3_and_3() {
        // len 7..=12：首3 + ** + 尾3
        assert_eq!(mask_key("sk-abcdef"), "sk-**def");
        assert_eq!(mask_key("1234567890ab"), "123**0ab");
    }

    #[test]
    fn long_key_shows_6_and_6() {
        // len > 12：首6 + ** + 尾6
        // "sk-prj-1234567890abcdef"（23 字符）→ 首6 "sk-prj" + ** + 尾6 "abcdef"
        assert_eq!(mask_key("sk-prj-1234567890abcdef"), "sk-prj**abcdef");
    }

    #[test]
    fn multibyte_chars_not_corrupted() {
        // emoji / 中文按 Unicode scalar 切分，不会切坏
        // "🔑secret🔑"（8 字符）→ 落在 7..=12 档：首3 "🔑se" + ** + 尾3 "et🔑"
        assert_eq!(mask_key("🔑secret🔑"), "🔑se**et🔑");
    }
}
