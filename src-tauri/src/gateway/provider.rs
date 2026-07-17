use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
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
    pub extra_headers: Vec<(String, String)>,
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
            extra_headers: cfg
                .extra_headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
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
        // 预构建透传 + 补充请求头（auth 单独 per-key 处理）：
        //   1. 透传 incoming 所有非认证 / 非 hop-by-hop 头（小写比较）
        //   2. extra_headers 仅补充 consumer 未带的 key（不覆盖已透传的值）
        //   认证头（authorization / x-api-key / anthropic-version）由 auth_headers_for 注入，不透传
        let forward = build_forward_headers(incoming, &self.extra_headers);
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
                // 构建完整上游请求头：auth（优先级最低）→ 透传+补充（forward，覆盖同名 auth 以防用户故意覆盖认证）
                let mut headers = self.auth_headers_for(key);
                for (name, val) in &forward {
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
    let len = key.chars().count();
    if len <= 12 {
        // 短 key：只保留首 4 个字符（不足则全部），后接 **
        let head: String = key.chars().take(4).collect();
        return format!("{head}**");
    }
    let head: String = key.chars().take(6).collect();
    let tail: String = key
        .chars()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}**{tail}")
}

/// 构建透传到上游的请求头集合：
///
/// 1. 透传 incoming 所有非认证 / 非 hop-by-hop 头（小写比较）
/// 2. extra_headers 仅补充 consumer 未携带的 key（不覆盖已透传的值）
/// 3. 认证头（authorization / x-api-key / anthropic-version）由 auth_headers_for 注入，不透传
fn build_forward_headers(
    incoming: &HeaderMap,
    extra: &[(String, String)],
) -> Vec<(HeaderName, HeaderValue)> {
    // 跳过：认证 + 协议/传输相关（hop-by-hop & reqwest 自动设置）+ 隐私敏感头
    // - 认证：由 auth_headers_for 单独注入，避免 consumer 的认证头泄漏给上游
    // - hop-by-hop：HTTP/1.1 规范要求代理剥离的头
    // - cookie / forwarded：consumer 端的会话信息，禁止透传给第三方上游
    const SKIP: &[&str] = &[
        "authorization",
        "x-api-key",
        "anthropic-version",
        "host",
        "content-length",
        "content-type",
        "connection",
        "keep-alive",
        "proxy-authorization",
        "proxy-authenticate",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        // 会话/隐私头：禁止泄漏给第三方上游
        "cookie",
        "cookie2",
        // 代理/转发链路头：上游不应感知 consumer 端的代理路径
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-port",
        "x-real-ip",
        // 安全相关头：不应被 consumer 覆盖上游的安全策略
        "strict-transport-security",
        "content-security-policy",
        "x-content-type-options",
        "x-frame-options",
    ];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<(HeaderName, HeaderValue)> = Vec::new();
    for (name, val) in incoming.iter() {
        let lname = name.as_str().to_lowercase();
        if SKIP.contains(&lname.as_str()) {
            continue;
        }
        // 跳过 consumer 自身的 consumer key（防泄漏），但网关本身不校验这些头，保留即可
        seen.insert(lname.clone());
        out.push((name.clone(), val.clone()));
    }
    // extra_headers 补充：仅添加 consumer 未携带的 key
    for (k, v) in extra {
        if seen.contains(&k.to_lowercase()) {
            continue;
        }
        if let (Ok(name), Ok(val)) = (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v)) {
            seen.insert(k.to_lowercase());
            out.push((name, val));
        }
    }
    out
}
