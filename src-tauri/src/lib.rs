//! Tauri 应用入口：网关启停控制、配置管理、系统托盘、开机自启

mod config;
mod converter;
mod db;
mod error;
mod handler;
mod log_bridge;
mod provider;
mod router;
mod stream;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use config::Config;
use provider::{KeyStatus, Provider};

/// 网关运行时句柄
struct ServerHandle {
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
    join: tauri::async_runtime::JoinHandle<()>,
}

/// 全局共享状态（Tauri .manage() 注册）
struct AppCtrl {
    config: Mutex<Config>,
    db: Mutex<rusqlite::Connection>,
    server: Mutex<Option<ServerHandle>>,
    listen_addr: Mutex<String>,
    providers: Mutex<Vec<Arc<Provider>>>,
    log_level_setter: log_bridge::LogLevelSetter,
}

/// 托盘菜单项引用（供运行时更新文本）
struct TrayItems {
    status: MenuItem<tauri::Wry>,
    toggle_gw: MenuItem<tauri::Wry>,
}

/// 网关状态（IPC 返回）
#[derive(Serialize)]
struct GatewayStatus {
    running: bool,
    listen_addr: String,
}

/// 单个 provider 运行时状态（IPC 返回）
#[derive(Serialize)]
struct ProviderRuntime {
    name: String,
    enabled: bool,
    protocol: String,
    base_url: String,
    models: Vec<String>,
    keys: Vec<KeyStatus>,
}

// ===================== 内部函数（取 &AppHandle，避免 MutexGuard 跨 await）=====================

/// 从配置构建 providers（只包含 enabled 的）
fn build_providers(cfg: &Config) -> anyhow::Result<Vec<Arc<Provider>>> {
    let mut providers = Vec::new();
    for pc in &cfg.providers {
        if !pc.enabled {
            continue;
        }
        let has_enabled_key = pc.api_keys.iter().any(|k| k.enabled());
        if !has_enabled_key {
            tracing::warn!(provider = %pc.name, "跳过：无启用的 key");
            continue;
        }
        let p = Provider::new(pc, cfg.key_blacklist_secs)?;
        providers.push(Arc::new(p));
    }
    Ok(providers)
}

/// 关闭 server（提取出 handle 后 await，不持锁）
async fn shutdown_server(handle: ServerHandle) {
    let _ = handle.shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

/// 启动网关（内部，取 &AppHandle）
async fn start_gateway_inner(app: &tauri::AppHandle) -> Result<String, String> {
    let ctrl = app.state::<AppCtrl>();

    // 检查是否已运行
    {
        let server = ctrl.server.lock().unwrap();
        if server.is_some() {
            return Err("网关已在运行".into());
        }
    }

    // 同步构建
    let cfg = ctrl.config.lock().unwrap().clone();
    let providers = build_providers(&cfg).map_err(|e| e.to_string())?;
    let app_state = config::AppState::build(&cfg, providers.clone()).map_err(|e| e.to_string())?;
    let app_router = router::build(app_state);

    // 异步绑定（不持有任何 MutexGuard）
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| format!("绑定 {} 失败: {e}", cfg.listen))?;

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let join = tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, app_router)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await;
    });

    // 同步存储结果
    let addr = cfg.listen.clone();
    *ctrl.server.lock().unwrap() = Some(ServerHandle {
        shutdown_tx: tx,
        join,
    });
    *ctrl.listen_addr.lock().unwrap() = addr.clone();
    *ctrl.providers.lock().unwrap() = providers;

    tracing::info!(addr = %addr, "网关已启动");
    update_tray(app, true);
    Ok(addr)
}

/// 停止网关（内部）
async fn stop_gateway_inner(app: &tauri::AppHandle) -> Result<(), String> {
    let ctrl = app.state::<AppCtrl>();

    // 提取 handle（锁内取，锁外 await）
    let handle = ctrl.server.lock().unwrap().take();
    if let Some(h) = handle {
        shutdown_server(h).await;
    }
    *ctrl.providers.lock().unwrap() = Vec::new();
    tracing::info!("网关已停止");
    update_tray(app, false);
    Ok(())
}

/// 如果网关正在运行则全量重建
async fn rebuild_if_running(app: &tauri::AppHandle) -> Result<(), String> {
    let ctrl = app.state::<AppCtrl>();
    let running = ctrl.server.lock().unwrap().is_some();
    if !running {
        return Ok(());
    }
    // 提取 handle 后 await
    let handle = ctrl.server.lock().unwrap().take();
    if let Some(h) = handle {
        shutdown_server(h).await;
    }
    drop(ctrl); // 释放 State 引用

    // 重新启动
    start_gateway_inner(app).await?;
    Ok(())
}

// ===================== Tauri Commands =====================

#[tauri::command]
fn get_config(app: tauri::AppHandle) -> Config {
    let ctrl = app.state::<AppCtrl>();
    let mut cfg = ctrl.config.lock().unwrap().clone();
    sync_consumer_models(&mut cfg);
    cfg
}

/// 计算 consumer models = 所有已启用 provider 的模型并集
fn compute_consumer_models(cfg: &Config) -> Vec<String> {
    let mut models: Vec<String> = Vec::new();
    for p in &cfg.providers {
        if p.enabled {
            for m in &p.models {
                if !models.contains(m) {
                    models.push(m.clone());
                }
            }
        }
    }
    models
}

/// 保存前自动同步 consumer.models
fn sync_consumer_models(cfg: &mut Config) {
    cfg.consumer.models = compute_consumer_models(cfg);
}

#[tauri::command]
async fn save_config(app: tauri::AppHandle, mut cfg: Config) -> Result<(), String> {
    sync_consumer_models(&mut cfg);
    {
        let ctrl = app.state::<AppCtrl>();
        let old_level = ctrl.config.lock().unwrap().log.level.clone();
        db::save_config(&ctrl.db.lock().unwrap(), &cfg).map_err(|e| e.to_string())?;
        let mut guard = ctrl.config.lock().unwrap();
        if cfg.log.level != old_level {
            ctrl.log_level_setter.set(&cfg.log.level);
        }
        *guard = cfg;
    }
    rebuild_if_running(&app).await?;
    Ok(())
}

#[tauri::command]
async fn start_gateway(app: tauri::AppHandle) -> Result<String, String> {
    start_gateway_inner(&app).await
}

#[tauri::command]
async fn stop_gateway(app: tauri::AppHandle) -> Result<(), String> {
    stop_gateway_inner(&app).await
}

#[tauri::command]
fn gateway_status(app: tauri::AppHandle) -> GatewayStatus {
    let ctrl = app.state::<AppCtrl>();
    let running = ctrl.server.lock().unwrap().is_some();
    let listen_addr = ctrl.listen_addr.lock().unwrap().clone();
    GatewayStatus { running, listen_addr }
}

#[tauri::command]
async fn toggle_provider(app: tauri::AppHandle, name: String, enabled: bool) -> Result<(), String> {
    {
        let ctrl = app.state::<AppCtrl>();
        let mut cfg = ctrl.config.lock().unwrap();
        for p in &mut cfg.providers {
            if p.name == name {
                p.enabled = enabled;
                break;
            }
        }
        sync_consumer_models(&mut cfg);
        db::save_config(&ctrl.db.lock().unwrap(), &cfg).map_err(|e| e.to_string())?;
    }
    rebuild_if_running(&app).await?;
    tracing::info!(provider = %name, enabled, "provider 已切换");
    Ok(())
}

#[tauri::command]
async fn toggle_key(
    app: tauri::AppHandle,
    provider_name: String,
    key_idx: usize,
    enabled: bool,
) -> Result<(), String> {
    {
        let ctrl = app.state::<AppCtrl>();
        let mut cfg = ctrl.config.lock().unwrap();
        for p in &mut cfg.providers {
            if p.name == provider_name {
                if key_idx < p.api_keys.len() {
                    p.api_keys[key_idx].set_enabled(enabled);
                }
                break;
            }
        }
        db::save_config(&ctrl.db.lock().unwrap(), &cfg).map_err(|e| e.to_string())?;
    }
    rebuild_if_running(&app).await?;
    tracing::info!(provider = %provider_name, key_idx, enabled, "key 已切换");
    Ok(())
}

#[tauri::command]
fn runtime_status(app: tauri::AppHandle) -> Vec<ProviderRuntime> {
    let ctrl = app.state::<AppCtrl>();
    let cfg = ctrl.config.lock().unwrap();
    let providers = ctrl.providers.lock().unwrap();

    cfg.providers
        .iter()
        .map(|pc| {
            let runtime_keys: Vec<KeyStatus> = providers
                .iter()
                .find(|p| p.name == pc.name)
                .map(|p| p.key_statuses())
                .unwrap_or_default();

            ProviderRuntime {
                name: pc.name.clone(),
                enabled: pc.enabled,
                protocol: format!("{:?}", pc.protocol).to_lowercase(),
                base_url: pc.base_url.clone(),
                models: pc.models.clone(),
                keys: runtime_keys,
            }
        })
        .collect()
}

#[tauri::command]
fn enable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    app.autolaunch().enable().map_err(|e| e.to_string())
}

#[tauri::command]
fn disable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    app.autolaunch().disable().map_err(|e| e.to_string())
}

#[tauri::command]
fn autostart_status(app: tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

// ===================== 系统托盘 =====================

/// 更新托盘状态文本
fn update_tray(app: &tauri::AppHandle, running: bool) {
    if let Some(ts) = app.try_state::<TrayItems>() {
        let _ = ts.status.set_text(if running { "状态: 运行中" } else { "状态: 已停止" });
        let _ = ts.toggle_gw.set_text(if running { "停止网关" } else { "启动网关" });
    }
    if let Some(tray) = app.tray_by_id("main-tray") {
        let tip = if running { "AI 聚合网关 - 运行中" } else { "AI 聚合网关 - 已停止" };
        let _ = tray.set_tooltip(Some(tip));
    }
    let _ = app.emit("gateway-state-changed", running);
}

/// 构建系统托盘，返回需要后续更新的菜单项
fn build_tray(app: &tauri::AppHandle) -> tauri::Result<TrayItems> {
    let status_item = MenuItem::with_id(app, "status", "状态: 已停止", false, None::<&str>)?;
    let show_item = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let toggle_item = MenuItem::with_id(app, "toggle_gw", "启动网关", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;

    let menu = Menu::with_items(app, &[
        &status_item, &sep1, &show_item, &toggle_item, &sep2, &quit_item,
    ])?;

    let _tray = TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("AI 聚合网关 - 已停止")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "toggle_gw" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let running = app.state::<AppCtrl>().server.lock().unwrap().is_some();
                    if running {
                        let _ = stop_gateway_inner(&app).await;
                    } else {
                        let _ = start_gateway_inner(&app).await;
                    }
                });
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;

    Ok(TrayItems {
        status: status_item,
        toggle_gw: toggle_item,
    })
}

// ===================== 应用入口 =====================

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let log_slot = log_bridge::create_slot();

    // 日志目录：可执行文件所在目录下的 ./logs/
    let log_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_level_setter = log_bridge::install("info", log_slot.clone(), log_dir.clone());
    // 启动时清理超过 30 天或总大小超 10GB 的旧日志
    log_bridge::purge_old_logs(&log_dir, 30, 10 * 1024 * 1024 * 1024);
    // 每天定时清理一次
    let purge_dir = log_dir.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
            log_bridge::purge_old_logs(&purge_dir, 30, 10 * 1024 * 1024 * 1024);
        }
    });
    tracing::info!("日志系统启动，文件日志写入 ./logs/ 目录，按天+按大小(10MB)双滚动，gzip 归档，保留 30 天，总大小上限 10GB");

    // 数据库文件：可执行文件所在目录下的 ./data/config.db
    let db_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("data");
    let _ = std::fs::create_dir_all(&db_dir);
    let db_path = db_dir.join("config.db");
    let db_path_str = db_path.to_string_lossy().to_string();

    let conn = db::open(&db_path_str).unwrap_or_else(|e| {
        panic!("打开数据库 {db_path_str} 失败: {e}");
    });
    db::init_tables(&conn).expect("初始化数据库表失败");

    let cfg = db::load_config(&conn).unwrap_or_else(|e| {
        tracing::error!(err = %e, "从数据库加载配置失败，使用空配置");
        config::default_config()
    });
    tracing::info!(db = %db_path_str, providers = cfg.providers.len(), "配置已加载");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(AppCtrl {
            config: Mutex::new(cfg),
            db: Mutex::new(conn),
            server: Mutex::new(None),
            listen_addr: Mutex::new(String::new()),
            providers: Mutex::new(Vec::new()),
            log_level_setter,
        })
        .setup(move |app| {
            log_bridge::set_app_handle(&log_slot, app.handle().clone());

            let tray_items = build_tray(app.handle())?;
            app.manage(tray_items);

            // 开机自启时隐藏窗口
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口 → 隐藏到托盘（不退出）
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            start_gateway,
            stop_gateway,
            gateway_status,
            toggle_provider,
            toggle_key,
            runtime_status,
            enable_autostart,
            disable_autostart,
            autostart_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
