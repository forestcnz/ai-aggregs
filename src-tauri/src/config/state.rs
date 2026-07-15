use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::types::{Config, Protocol};
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
    /// model -> 上次成功的 provider id，下次路由时优先
    pub last_provider: Arc<Mutex<HashMap<String, i64>>>,
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
            last_provider: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn route(&self, model: &str, c_proto: Protocol) -> Option<Vec<Arc<Provider>>> {
        let idxs = self.model_map.get(model)?;
        if idxs.is_empty() {
            return None;
        }
        // 上次成功的 provider id
        let last_id = self.last_provider.lock().unwrap().get(model).copied();

        // 四级优先：协议匹配+上次使用 > 协议匹配+其他 > 协议不匹配+上次使用 > 协议不匹配+其他
        let mut a = Vec::new(); // 协议匹配 + 上次使用
        let mut b = Vec::new(); // 协议匹配 + 其他
        let mut c = Vec::new(); // 协议不匹配 + 上次使用
        let mut d = Vec::new(); // 协议不匹配 + 其他
        for &i in idxs {
            let proto_match = self.providers[i].protocol == c_proto;
            let is_last = last_id == Some(self.providers[i].id);
            match (proto_match, is_last) {
                (true, true) => a.push(i),
                (true, false) => b.push(i),
                (false, true) => c.push(i),
                (false, false) => d.push(i),
            }
        }
        let mut ordered = a;
        ordered.extend(b);
        ordered.extend(c);
        ordered.extend(d);
        Some(ordered.into_iter().map(|i| self.providers[i].clone()).collect())
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
