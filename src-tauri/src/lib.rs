//! Tauri 应用入口：初始化日志、数据库、注册命令、构建托盘

#![warn(clippy::all)]
#![warn(clippy::dbg_macro, clippy::todo)]

mod api;
mod config;
mod gateway;
mod infra;

use std::sync::Mutex;

use tauri::Manager;
use tauri_plugin_autostart::MacosLauncher;

use crate::api::commands::*;
use crate::config::state::AppCtrl;
use crate::config::types::default_config;
use crate::gateway::manager::start_gateway_inner;
use crate::infra::db;
use crate::infra::log_bridge;
use crate::infra::tray::build_tray;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ---- 日志系统初始化 ----
    let log_slot = log_bridge::create_slot();

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let log_dir = exe_dir.join("logs");
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

    tracing::info!(
        "日志系统启动，文件日志写入 ./logs/ 目录，按天+按大小(10MB)双滚动，gzip 归档，保留 30 天，总大小上限 10GB"
    );

    // ---- 数据库初始化 ----
    let db_dir = exe_dir.join("data");
    let _ = std::fs::create_dir_all(&db_dir);
    let db_path = db_dir.join("config.db");
    let db_path_str = db_path.to_string_lossy().to_string();

    let conn = db::open(&db_path_str).unwrap_or_else(|e| {
        panic!("打开数据库 {db_path_str} 失败: {e}");
    });
    db::init_tables(&conn).expect("初始化数据库表失败");

    let cfg = db::load_config(&conn).unwrap_or_else(|e| {
        tracing::error!(err = %e, "从数据库加载配置失败，使用空配置");
        default_config()
    });
    tracing::info!(db = %db_path_str, providers = cfg.providers.len(), "配置已加载");

    // 应用配置中的日志级别（install 时用 info 占位，这里纠正为用户配置值）
    log_level_setter.set(&cfg.log.level);

    // ---- Tauri 应用构建 ----
    tauri::Builder::default()
        // 单实例限制（仅桌面端）：二次启动时聚焦到已运行实例而非新开进程
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // 二次启动时显示并聚焦主窗口
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .manage(AppCtrl {
            config: Mutex::new(cfg),
            db: std::sync::Arc::new(Mutex::new(conn)),
            server: Mutex::new(None),
            listen_addr: Mutex::new(String::new()),
            providers: Mutex::new(Vec::new()),
            log_level_setter,
        })
        .setup(move |app| {
            log_bridge::set_app_handle(&log_slot, app.handle().clone());

            let tray_items = build_tray(app.handle())?;
            app.manage(tray_items);

            // 按配置自动恢复网关运行状态
            let should_start = {
                let ctrl = app.state::<AppCtrl>();
                let auto = ctrl.config.lock().unwrap().auto_start_gateway;
                let last_running = db::get_setting(&ctrl.db.lock().unwrap(), "gateway_running")
                    .map(|v| v == "1")
                    .unwrap_or(false);
                auto && last_running
            };
            if should_start {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match start_gateway_inner(&app_handle).await {
                        Ok(addr) => tracing::info!(addr = %addr, "网关已自动恢复启动"),
                        Err(e) => tracing::error!(err = %e, "自动启动网关失败"),
                    }
                });
            }

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
            get_usage,
            get_provider_usage,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
