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
use crate::infra::db;
use crate::infra::log_bridge;
use crate::infra::tray::build_tray;

// ===================== 日志/用量清理参数（集中常量） =====================
/// 日志保留天数
const LOG_RETENTION_DAYS: u64 = 30;
/// 日志目录总大小上限（字节）：10 GB
const LOG_MAX_TOTAL_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// 日志清理后台任务执行间隔：24 小时
const LOG_PURGE_INTERVAL_SECS: u64 = 24 * 60 * 60;

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

    // 启动时清理超过 LOG_RETENTION_DAYS 天或总大小超 LOG_MAX_TOTAL_BYTES 的旧日志
    log_bridge::purge_old_logs(&log_dir, LOG_RETENTION_DAYS, LOG_MAX_TOTAL_BYTES);

    // 每天定时清理一次
    let purge_dir = log_dir.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(LOG_PURGE_INTERVAL_SECS)).await;
            log_bridge::purge_old_logs(&purge_dir, LOG_RETENTION_DAYS, LOG_MAX_TOTAL_BYTES);
        }
    });

    tracing::info!(
        retention_days = LOG_RETENTION_DAYS,
        max_total_bytes = LOG_MAX_TOTAL_BYTES,
        "日志系统启动，文件日志写入 ./logs/ 目录，按天+按大小(10MB)双滚动，gzip 归档，保留 {} 天，总大小上限 {} GB",
        LOG_RETENTION_DAYS,
        LOG_MAX_TOTAL_BYTES / 1024 / 1024 / 1024
    );

    // ---- 数据库初始化 ----
    let db_dir = exe_dir.join("data");
    let _ = std::fs::create_dir_all(&db_dir);
    let db_path = db_dir.join("config.db");
    let db_path_str = db_path.to_string_lossy().to_string();

    let conn = match db::open(&db_path_str) {
        Ok(c) => c,
        Err(e) => {
            // 数据库打开失败不 panic：写崩溃日志到 exe 同级目录，便于用户反馈问题
            let msg = format!("打开数据库 {db_path_str} 失败: {e}");
            tracing::error!(err = %msg, "数据库初始化失败");
            write_crash_log(&exe_dir, &msg);
            std::process::exit(1);
        }
    };
    if let Err(e) = db::init_tables(&conn) {
        let msg = format!("初始化数据库表失败: {e}");
        tracing::error!(err = %msg, "数据库表初始化失败");
        write_crash_log(&exe_dir, &msg);
        std::process::exit(1);
    }

    let cfg = db::load_config(&conn).unwrap_or_else(|e| {
        tracing::error!(err = %e, "从数据库加载配置失败，使用空配置");
        default_config()
    });
    tracing::info!(db = %db_path_str, providers = cfg.providers.len(), "配置已加载");

    // 安全提示：未配置 consumer key 时，本机任意进程可调用网关消耗上游额度
    if cfg.consumer.api_keys.is_empty() {
        tracing::warn!(
            "未配置 consumer api_keys：本机任意进程均可无鉴权调用网关。建议在「设置」页配置至少一个 key"
        );
    }

    // 应用配置中的日志级别（install 时用 info 占位，这里纠正为用户配置值）
    log_level_setter.set(&cfg.log.level);

    // 把 connection 包成 Arc<Mutex>，AppCtrl 共享同一份
    let db_conn = std::sync::Arc::new(Mutex::new(conn));

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
            db: db_conn,
            server: Mutex::new(None),
            listen_addr: Mutex::new(String::new()),
            providers: Mutex::new(Vec::new()),
            log_level_setter,
            last_model: std::sync::Arc::new(Mutex::new(std::collections::HashMap::new())),
        })
        .setup(move |app| {
            log_bridge::set_app_handle(&log_slot, app.handle().clone());

            let tray_items = build_tray(app.handle())?;
            app.manage(tray_items);

            // 网关自动恢复改由前端在页面就绪（ready）后通过
            // autostart_gateway_if_configured 触发，避免网关先于界面就绪。

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
            autostart_gateway_if_configured,
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
            last_used_models,
            opencode_config_load,
            opencode_config_save,
            opencode_provider_ids,
            opencode_version,
            claude_code_config_load,
            claude_code_config_save,
            claude_code_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 将致命错误写入 exe 同级目录的 crash.log，方便用户反馈问题（不使用 panic，避免终端无输出）
fn write_crash_log(exe_dir: &std::path::Path, msg: &str) {
    let path = exe_dir.join("crash.log");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("[{now}] {msg}\n");
    // 用 OpenOptions 追加写，保留历史崩溃记录
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}
