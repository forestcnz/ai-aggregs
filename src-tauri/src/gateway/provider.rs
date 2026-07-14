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
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub models: Vec<String>,
    pub max_retries: u32,
    pub extra_headers: Vec<(String, String)>,
    pub reasoning_effort: Option<String>,
    #[allow(dead_code)]
    pub enabled: bool,
    keys: Vec<ApiKeyEntry>,
    blacklist: Mutex<HashMap<usize, Instant>>,
    blacklist_disabled_until: Mutex<Option<Instant>>,
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
        match map.get(&idx) {
            Some(until) if *until > now => true,
            _ => false,
        }
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
    ) -> Result<reqwest::Response, UpstreamError> {
        let url = format!("{}{}", self.base_url, endpoint);
        let send_body: serde_json::Value = if let Some(eff) = &self.reasoning_effort {
            let mut b = body.clone();
            self.inject_reasoning(&mut b, eff);
            b
        } else {
            body.clone()
        };
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
                if !self.keys[idx].enabled() {
                    continue;
                }
                let now = Instant::now();
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
