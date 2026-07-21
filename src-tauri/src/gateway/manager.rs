use std::sync::Arc;
use std::time::Duration;

use tauri::{Emitter, Manager};

use crate::api::router;
use crate::config::state::{AppCtrl, AppState, ServerHandle};
use crate::config::types::Config;
use crate::gateway::provider::Provider;
use crate::infra::db;
use crate::infra::error::IpcError;

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

pub async fn shutdown_server(handle: ServerHandle) {
    let _ = handle.shutdown_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), handle.join).await;
}

pub async fn start_gateway_inner(app: &tauri::AppHandle) -> Result<String, IpcError> {
    let ctrl = app.state::<AppCtrl>();

    {
        let server = ctrl.server.lock().unwrap();
        if server.is_some() {
            return Err(IpcError::new("网关已在运行"));
        }
    }

    let mut cfg = ctrl.config.lock().unwrap().clone();
    sync_consumer_models(&mut cfg);
    let providers = build_providers(&cfg)?;
    let db = ctrl.db.clone();
    let app_state = AppState::build(
        &cfg,
        providers.clone(),
        db,
        ctrl.last_model.clone(),
        ctrl.metrics.clone(),
    )?;
    let app_router = router::build(app_state);

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

    let addr = cfg.listen.clone();
    *ctrl.server.lock().unwrap() = Some(ServerHandle {
        shutdown_tx: tx,
        join,
    });
    *ctrl.listen_addr.lock().unwrap() = addr.clone();
    *ctrl.providers.lock().unwrap() = providers;

    tracing::info!(addr = %addr, "网关已启动");
    let _ = app.emit("gateway-state-changed", true);
    let _ = db::set_setting(&ctrl.db.lock().unwrap(), "gateway_running", "1");
    Ok(addr)
}

pub async fn stop_gateway_inner(app: &tauri::AppHandle) -> Result<(), IpcError> {
    let ctrl = app.state::<AppCtrl>();

    let handle = ctrl.server.lock().unwrap().take();
    if let Some(h) = handle {
        shutdown_server(h).await;
    }
    *ctrl.providers.lock().unwrap() = Vec::new();
    let _ = db::set_setting(&ctrl.db.lock().unwrap(), "gateway_running", "0");
    tracing::info!("网关已停止");
    let _ = app.emit("gateway-state-changed", false);
    Ok(())
}

#[allow(clippy::drop_non_drop)]
pub async fn rebuild_if_running(app: &tauri::AppHandle) -> Result<(), IpcError> {
    let ctrl = app.state::<AppCtrl>();
    let running = ctrl.server.lock().unwrap().is_some();
    if !running {
        return Ok(());
    }
    let handle = ctrl.server.lock().unwrap().take();
    if let Some(h) = handle {
        shutdown_server(h).await;
    }
    drop(ctrl);

    start_gateway_inner(app).await?;
    Ok(())
}

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
    // 追加已启用的模型映射别名（对外可请求的虚拟模型）
    for mm in &cfg.model_mappings {
        if mm.enabled {
            let alias = mm.alias.trim();
            if !alias.is_empty() && !models.iter().any(|m| m == alias) {
                models.push(alias.to_string());
            }
        }
    }
    models
}

pub fn sync_consumer_models(cfg: &mut Config) {
    cfg.consumer.models = compute_consumer_models(cfg);
}
