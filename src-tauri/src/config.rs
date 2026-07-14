//! 配置定义 / 加载 / 校验，以及运行时 AppState、Consumer、Protocol
//!
//! Tauri 改造：所有结构体派生 Serialize（供 IPC 传输）；
//! api_keys 改为 ApiKeyEntry（兼容旧字符串格式 + 新 {key,enabled} 对象格式）；
//! ProviderConfig 新增 enabled 字段。

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::provider::Provider;

/// 协议类型：一个 Provider 固定一种；Consumer 对外三种协议均支持（由请求路径判定）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Chat,
    Responses,
    Anthropic,
}

impl Protocol {
    /// 该协议对应的上游端点路径
    pub fn endpoint(self) -> &'static str {
        match self {
            Protocol::Chat => "/chat/completions",
            Protocol::Responses => "/responses",
            Protocol::Anthropic => "/messages",
        }
    }

    /// 转字符串（用于 SQLite 存储）
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Chat => "chat",
            Protocol::Responses => "responses",
            Protocol::Anthropic => "anthropic",
        }
    }

    /// 从字符串解析
    pub fn from_str(s: &str) -> Self {
        match s {
            "responses" => Protocol::Responses,
            "anthropic" => Protocol::Anthropic,
            _ => Protocol::Chat,
        }
    }
}

/// API Key 条目：兼容两种格式
/// - 旧格式（纯字符串）："sk-xxx" → 读取时 enabled 默认 true
/// - 新格式（对象）：{ key: "sk-xxx", enabled: true }
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ApiKeyEntry {
    Object { key: String, enabled: bool },
    Plain(String),
}

impl ApiKeyEntry {
    pub fn key(&self) -> &str {
        match self {
            ApiKeyEntry::Object { key, .. } => key,
            ApiKeyEntry::Plain(k) => k,
        }
    }
    pub fn enabled(&self) -> bool {
        match self {
            ApiKeyEntry::Object { enabled, .. } => *enabled,
            ApiKeyEntry::Plain(_) => true,
        }
    }
    pub fn set_enabled(&mut self, val: bool) {
        match self {
            ApiKeyEntry::Object { key: _, enabled } => *enabled = val,
            ApiKeyEntry::Plain(k) => {
                // 升级为 Object 格式
                *self = ApiKeyEntry::Object {
                    key: k.clone(),
                    enabled: val,
                };
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub listen: String,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    pub consumer: ConsumerConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// key 429 后加入黑名单的时长（秒），默认 600（10 分钟）
    #[serde(default = "default_blacklist_secs")]
    pub key_blacklist_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub api_keys: Vec<ApiKeyEntry>,
    pub models: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default = "default_retries")]
    pub max_retries: u32,
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// provider 是否启用（缺省 = true，向后兼容）
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 固定思考强度（注入到发给上游的请求体）。
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsumerConfig {
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LogConfig {
    #[serde(default = "default_level")]
    pub level: String,
}

fn default_timeout() -> u64 {
    120
}
fn default_retries() -> u32 {
    2
}
fn default_level() -> String {
    "info".into()
}
fn default_blacklist_secs() -> u64 {
    600
}
fn default_true() -> bool {
    true
}

/// 默认配置（首次创建时使用）
pub fn default_config() -> Config {
    Config {
        listen: "127.0.0.1:8000".into(),
        providers: vec![],
        consumer: ConsumerConfig {
            api_keys: vec![],
            models: vec![],
        },
        log: LogConfig {
            level: "info".into(),
        },
        key_blacklist_secs: 600,
    }
}

// ---------- 运行时状态 ----------

/// Consumer 门面：允许的 Key（三种协议端点均可访问）
#[derive(Clone)]
pub struct Consumer {
    pub api_keys: Vec<String>,
    pub models: Vec<String>,
}

impl Consumer {
    pub fn check_key(&self, presented: &str) -> bool {
        if self.api_keys.is_empty() {
            return true;
        }
        self.api_keys.iter().any(|k| k == presented)
    }
}

/// 全局共享状态，Axum 作为 State 传递
#[derive(Clone)]
pub struct AppState {
    pub consumer: Consumer,
    pub providers: Arc<Vec<Arc<Provider>>>,
    pub model_map: Arc<HashMap<String, Vec<usize>>>,
}

impl AppState {
    pub fn build(cfg: &Config, providers: Vec<Arc<Provider>>) -> anyhow::Result<Self> {
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in providers.iter().enumerate() {
            for m in &p.models {
                map.entry(m.clone()).or_default().push(i);
            }
        }
        Ok(Self {
            consumer: Consumer {
                api_keys: cfg.consumer.api_keys.clone(),
                models: cfg.consumer.models.clone(),
            },
            providers: Arc::new(providers),
            model_map: Arc::new(map),
        })
    }

    /// model -> 候选 provider 列表（固定按配置顺序，第一个优先）
    pub fn route(&self, model: &str) -> Option<Vec<Arc<Provider>>> {
        let idxs = self.model_map.get(model)?;
        if idxs.is_empty() {
            return None;
        }
        Some(idxs.iter().map(|i| self.providers[*i].clone()).collect())
    }
}
