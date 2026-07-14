//! Provider 运行时结构：Key Pool（顺序 failover）+ 429 黑名单 + 失败重试发送
//!
//! Tauri 改造：keys 存储 KeyEntry（含 enabled 标记），send 跳过 disabled key；
//! 新增 key_statuses() 供 GUI 查询每个 key 的运行时状态（正常/已禁用/已拉黑·倒计时）。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Serialize;

use crate::config::{ApiKeyEntry, Protocol, ProviderConfig};

/// 上游错误：保留原始 HTTP 状态码，便于网关透传给客户端
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

/// 单个 key 的运行时状态快照（供 GUI 展示）
#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub idx: usize,
    pub masked: String,
    pub enabled: bool,
    pub blacklisted: bool,
    /// 拉黑剩余秒数（仅 blacklisted=true 时有意义）
    pub blacklist_remaining_secs: Option<u64>,
}

/// 单个 Provider 的运行时表示（共享不可变，内部用 Mutex 维护可变状态）
pub struct Provider {
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub models: Vec<String>,
    pub max_retries: u32,
    pub extra_headers: Vec<(String, String)>,
    pub reasoning_effort: Option<String>,
    /// 是否启用（disabled provider 不参与路由）
    #[allow(dead_code)]
    pub enabled: bool,
    keys: Vec<ApiKeyEntry>,
    /// key index -> 解禁时间（只记录 429 拉黑的 key）
    blacklist: Mutex<HashMap<usize, Instant>>,
    /// 黑名单禁用到期时间：None = 正常生效；Some(until) = 在 until 之前所有 key 均不拉黑
    blacklist_disabled_until: Mutex<Option<Instant>>,
    /// 单个 key 429 后拉黑的秒数
    blacklist_secs: u64,
    client: reqwest::Client,
}

impl Provider {
    pub fn new(cfg: &ProviderConfig, blacklist_secs: u64) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()?;
        Ok(Self {
            name: cfg.name.clone(),
            protocol: cfg.protocol,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            models: cfg.models.clone(),
            max_retries: cfg.max_retries,
            extra_headers: cfg
                .extra_headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            reasoning_effort: cfg.reasoning_effort.clone(),
            enabled: cfg.enabled,
            keys: cfg.api_keys.clone(),
            blacklist: Mutex::new(HashMap::new()),
            blacklist_disabled_until: Mutex::new(None),
            blacklist_secs,
            client,
        })
    }

    /// 返回所有 key 的运行时状态快照（供 GUI 展示）
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

    /// 检查 key index 是否在当前黑名单中（未过期的才算拉黑）
    fn is_blacklisted(&self, idx: usize, now: Instant) -> bool {
        let map = self.blacklist.lock().unwrap();
        match map.get(&idx) {
            Some(until) if *until > now => true,
            _ => false,
        }
    }

    /// 将 key index 加入黑名单
    fn blacklist_key(&self, idx: usize, now: Instant) {
        let mut map = self.blacklist.lock().unwrap();
        map.insert(idx, now + Duration::from_secs(self.blacklist_secs));
    }

    /// 黑名单是否被全局禁用（降级期：所有 key 免拉黑）
    fn is_blacklist_disabled(&self, now: Instant) -> bool {
        let guard = self.blacklist_disabled_until.lock().unwrap();
        guard.map(|u| now < u).unwrap_or(false)
    }

    /// 判断是否所有启用的 key 都在黑名单中
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

    /// 组装某个 key 对应的鉴权头（按协议区分）
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

    /// 上游端点路径（按协议）
    pub fn endpoint(&self) -> &'static str {
        self.protocol.endpoint()
    }

    /// 发起一次（可能重试的）请求。
    /// 跳过 disabled key 和黑名单中的 key；429 拉黑该 key；
    /// 所有启用 key 均被拉黑时清空黑名单并全局禁用 10 分钟。
    pub async fn send(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        stream: bool,
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{}{}", self.base_url, endpoint);
        let send_body: serde_json::Value = if let Some(eff) = &self.reasoning_effort {
            let mut b = body.clone();
            self.inject_reasoning(&mut b, eff);
            b
        } else {
            body.clone()
        };
        // 启用的 key 数量决定最大尝试次数
        let enabled_count = self.keys.iter().filter(|e| e.enabled()).count();
        let total = ((self.max_retries as usize) + 1).min(enabled_count.max(1));
        let mut last_err: Option<UpstreamError> = None;

        loop {
            let now = Instant::now();
            let disabled = self.is_blacklist_disabled(now);
            let mut tried = 0usize;

            for idx in 0..self.keys.len() {
                if tried >= total {
                    break;
                }
                // 跳过手动禁用的 key
                if !self.keys[idx].enabled() {
                    continue;
                }
                let now = Instant::now();
                // 跳过黑名单中的 key（除非黑名单全局禁用）
                if !disabled && self.is_blacklisted(idx, now) {
                    continue;
                }

                let key = self.keys[idx].key();
                let masked = mask_key(key);
                let mut req = self
                    .client
                    .post(&url)
                    .headers(self.auth_headers_for(key))
                    .json(&send_body);
                if stream {
                    req = req.header("accept", "text/event-stream");
                }
                for (k, v) in &self.extra_headers {
                    if let (Ok(name), Ok(val)) =
                        (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v))
                    {
                        req = req.header(name, val);
                    }
                }

                tried += 1;

                match req.send().await {
                    Ok(r) if r.status().is_success() => {
                        tracing::info!(
                            provider = %self.name,
                            idx,
                            key = %masked,
                            status = %r.status(),
                            "upstream ok"
                        );
                        return Ok(r);
                    }
                    Ok(r) => {
                        let status = r.status();
                        let code = status.as_u16();
                        let text = r.text().await.unwrap_or_default();
                        if code == 429 {
                            // 429：拉黑当前 key，继续试下一个
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
                            // 4xx（非 429）是请求本身问题，不拉黑也不切换
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
                            // 5xx：记录错误，继续试下一个 key
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

            // 全部启用 key 都在黑名单且未禁用 → 清空黑名单 + 全局禁用 10 分钟
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

    /// 按协议把 reasoning_effort 注入请求体
    fn inject_reasoning(&self, body: &mut serde_json::Value, effort: &str) {
        use serde_json::json;
        match self.protocol {
            Protocol::Chat => {
                body["reasoning_effort"] = json!(effort);
                if body.get("thinking").is_none() {
                    body["thinking"] = json!({"type": "enabled"});
                }
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

/// 脱敏：只保留 key 前后各 6 位，中间用 ** 代替
fn mask_key(key: &str) -> String {
    let len = key.chars().count();
    if len <= 12 {
        return format!("{}**", &key[..len.min(4)]);
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
