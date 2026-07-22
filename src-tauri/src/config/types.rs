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
    /// 模型映射（别名 → 实际后端模型池）。用户请求别名时，重定向到池中的实际模型。
    #[serde(default)]
    pub model_mappings: Vec<ModelMapping>,
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
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 流式 keepalive 心跳间隔（秒）。0 或缺省 = 使用全局默认 15s。
    /// 心跳以 SSE 注释行 `: keepalive\n\n` 形式发送，防止 nginx/反代空闲断开。
    #[serde(default)]
    pub stream_keepalive_interval_secs: Option<u64>,
    /// 流式首字超时（秒）：从请求发出到第一个有效上游 chunk 的最长等待。
    /// 0 或缺省 = 使用全局默认 120s。reasoning model 长时间无输出时主动断开。
    #[serde(default)]
    pub stream_first_output_timeout_secs: Option<u64>,
    /// HTTP 代理 URL（如 `http://127.0.0.1:7890` 或 `socks5://127.0.0.1:1080`）。
    /// 设置后该 provider 的所有请求通过此代理发出。
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// 代理认证（可选），格式 `user:password`，仅当 proxy_url 设置时生效。
    #[serde(default)]
    pub proxy_auth: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsumerConfig {
    #[serde(default)]
    pub api_keys: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
}

/// 模型映射：把对外别名重定向到一组实际后端模型（负载均衡 / 故障转移）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelMapping {
    /// 对外别名 — 用户请求时填的模型名
    #[serde(default)]
    pub alias: String,
    /// 实际后端模型池（按顺序尝试，命中后端 providers 的模型）
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LogConfig {
    #[serde(default = "default_level")]
    pub level: String,
}

fn default_timeout() -> u64 {
    3000
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
        listen: "127.0.0.1:8849".into(),
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
        model_mappings: vec![],
    }
}
