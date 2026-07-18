use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue};
use serde::Serialize;

use crate::config::types::{ApiKeyEntry, Protocol, ProviderConfig};

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
    keys: Vec<ApiKeyEntry>,
    blacklist: Mutex<HashMap<usize, Instant>>,
    blacklist_disabled_until: Mutex<Option<Instant>>,
    blacklist_secs: u64,
    client: reqwest::Client,
    /// 非流式请求的总超时（流式请求不应用此超时，避免长 SSE 被切断）
    timeout: Duration,
    /// 上次成功使用的密钥索引，下次优先尝试
    last_key_idx: Mutex<Option<usize>>,
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
        Ok(Self {
            id: cfg.id,
            name: cfg.name.clone(),
            protocol: cfg.protocol,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            models: cfg.models.clone(),
            reasoning_effort: cfg.reasoning_effort.clone(),
            keys: cfg.api_keys.clone(),
            blacklist: Mutex::new(HashMap::new()),
            blacklist_disabled_until: Mutex::new(None),
            blacklist_secs,
            timeout: Duration::from_secs(cfg.timeout_secs),
            client,
            last_key_idx: Mutex::new(None),
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
        let mut h = HeaderMap::new();
        match self.protocol {
            Protocol::Chat | Protocol::Responses => {
                if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
                    h.insert("authorization", v);
                }
            }
            Protocol::Anthropic => {
                if let Ok(v) = HeaderValue::from_str(key) {
                    h.insert("x-api-key", v);
                }
                h.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
        }
        h
    }

    pub fn endpoint(&self) -> &'static str {
        self.protocol.endpoint()
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
        use serde_json::json;
        match self.protocol {
            Protocol::Chat => {
                body["reasoning_effort"] = json!(effort);
            }
            Protocol::Responses => {
                body["reasoning"] = json!({"effort": effort, "summary": "auto"});
            }
            Protocol::Anthropic => {
                if body.get("thinking").is_none() {
                    body["thinking"] = json!({"type": "enabled"});
                }
            }
        }
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
