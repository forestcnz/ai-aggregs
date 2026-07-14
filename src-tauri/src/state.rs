//! 全局共享状态定义（通过 Tauri `.manage()` 注册）

use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::config::Config;
use crate::log_bridge::LogLevelSetter;
use crate::provider::{KeyStatus, Provider};

/// 网关运行时句柄 — 持有 shutdown 信号和异步任务句柄
pub struct ServerHandle {
    pub shutdown_tx: tokio::sync::oneshot::Sender<()>,
    pub join: tauri::async_runtime::JoinHandle<()>,
}

/// 全局共享状态（Tauri `.manage()` 注册）
pub struct AppCtrl {
    pub config: Mutex<Config>,
    pub db: Mutex<rusqlite::Connection>,
    pub server: Mutex<Option<ServerHandle>>,
    pub listen_addr: Mutex<String>,
    pub providers: Mutex<Vec<Arc<Provider>>>,
    pub log_level_setter: LogLevelSetter,
}

/// 托盘菜单项引用（供运行时更新文本）
pub struct TrayItems {
    pub status: tauri::menu::MenuItem<tauri::Wry>,
    pub toggle_gw: tauri::menu::MenuItem<tauri::Wry>,
}

/// 网关状态（IPC `gateway_status` 命令返回）
#[derive(Serialize)]
pub struct GatewayStatus {
    pub running: bool,
    pub listen_addr: String,
}

/// 单个 provider 运行时状态（IPC `runtime_status` 命令返回）
#[derive(Serialize)]
pub struct ProviderRuntime {
    pub name: String,
    pub enabled: bool,
    pub protocol: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub keys: Vec<KeyStatus>,
}
