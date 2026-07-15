use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::types::Config;
use crate::gateway::provider::{KeyStatus, Provider};
use crate::infra::log_bridge::LogLevelSetter;

pub struct ServerHandle {
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub join: tauri::async_runtime::JoinHandle<()>,
}

pub struct AppCtrl {
    pub config: Mutex<Config>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub server: Mutex<Option<ServerHandle>>,
    pub listen_addr: Mutex<String>,
    pub providers: Mutex<Vec<Arc<Provider>>>,
    pub log_level_setter: LogLevelSetter,
}

pub struct TrayItems {
    pub status: tauri::menu::MenuItem<tauri::Wry>,
    pub toggle_gw: tauri::menu::MenuItem<tauri::Wry>,
}

#[derive(Serialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub listen_addr: String,
}

#[derive(Serialize)]
pub struct ProviderRuntime {
    pub name: String,
    pub enabled: bool,
    pub protocol: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub keys: Vec<KeyStatus>,
}

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

#[derive(Clone)]
pub struct AppState {
    pub consumer: Consumer,
    pub providers: Arc<Vec<Arc<Provider>>>,
    pub model_map: Arc<HashMap<String, Vec<usize>>>,
    pub db: Arc<Mutex<rusqlite::Connection>>,
}

impl AppState {
    pub fn build(
        cfg: &Config,
        providers: Vec<Arc<Provider>>,
        db: Arc<Mutex<rusqlite::Connection>>,
    ) -> anyhow::Result<Self> {
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
            db,
        })
    }

    pub fn route(&self, model: &str) -> Option<Vec<Arc<Provider>>> {
        let idxs = self.model_map.get(model)?;
        if idxs.is_empty() {
            return None;
        }
        Some(idxs.iter().map(|i| self.providers[*i].clone()).collect())
    }
}

// ===================== 用量统计 IPC 返回类型 =====================

/// 单个模型的聚合用量
#[derive(Serialize)]
pub struct UsageModelRow {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// 用量统计汇总（含各模型明细 + 总计）
#[derive(Serialize)]
pub struct UsageSummary {
    pub models: Vec<UsageModelRow>,
    pub total_requests: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_tokens: u64,
}
