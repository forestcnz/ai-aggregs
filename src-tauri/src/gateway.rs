//! 网关生命周期管理：启动、停止、重建

use std::sync::Arc;
use std::time::Duration;

use tauri::Manager;

use crate::config;
use crate::config::Config;
use crate::error::IpcError;
use crate::provider::Provider;
use crate::router;
use crate::state::AppCtrl;
use crate::tray::update_tray;

/// 从配置构建 providers（只包含 enabled 且有可用 key 的）
pub fn build_providers(cfg: &Config) -> anyhow::Result<Vec<Arc<Provider>>> {
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
pub async fn shutdown_server(handle: crate::state::ServerHandle) {
    let _ = handle.shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

/// 启动网关（内部，取 &AppHandle）
pub async fn start_gateway_inner(app: &tauri::AppHandle) -> Result<String, IpcError> {
    let ctrl = app.state::<AppCtrl>();

    // 检查是否已运行
    {
        let server = ctrl.server.lock().unwrap();
        if server.is_some() {
            return Err(IpcError::new("网关已在运行"));
        }
    }

    // 同步构建
    let cfg = ctrl.config.lock().unwrap().clone();
    let providers = build_providers(&cfg)?;
    let app_state = config::AppState::build(&cfg, providers.clone())?;
    let app_router = router::build(app_state);

    // 异步绑定（不持有任何 MutexGuard）
    let listener = tokio::net::TcpListener::bind(&cfg.listen)
        .await
        .map_err(|e| IpcError(format!("绑定 {} 失败: {e}", cfg.listen)))?;

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
    *ctrl.server.lock().unwrap() = Some(crate::state::ServerHandle {
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
pub async fn stop_gateway_inner(app: &tauri::AppHandle) -> Result<(), IpcError> {
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

/// 如果网关正在运行则全量重建（配置变更后调用）
pub async fn rebuild_if_running(app: &tauri::AppHandle) -> Result<(), IpcError> {
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

/// 计算 consumer models = 所有已启用 provider 的模型并集
pub fn compute_consumer_models(cfg: &Config) -> Vec<String> {
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
pub fn sync_consumer_models(cfg: &mut Config) {
    cfg.consumer.models = compute_consumer_models(cfg);
}
