use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Chat,
    Responses,
    Anthropic,
}

impl Protocol {
    pub fn endpoint(self) -> &'static str {
        match self {
            Protocol::Chat => "/chat/completions",
            Protocol::Responses => "/responses",
            Protocol::Anthropic => "/messages",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Chat => "chat",
            Protocol::Responses => "responses",
            Protocol::Anthropic => "anthropic",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "responses" => Protocol::Responses,
            "anthropic" => Protocol::Anthropic,
            _ => Protocol::Chat,
        }
    }
}

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
    #[serde(default = "default_blacklist_secs")]
    pub key_blacklist_secs: u64,
    /// 启动应用时是否恢复上次网关运行状态（需配合运行状态记录）
    #[serde(default)]
    pub auto_start_gateway: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// 数据库行 ID（仅后端使用，前端只读）
    #[serde(default)]
    pub id: i64,
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
    #[serde(default = "default_true")]
    pub enabled: bool,
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
        auto_start_gateway: false,
    }
}
