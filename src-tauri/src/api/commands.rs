use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;

use crate::config::state::{AppCtrl, GatewayStatus, ProviderRuntime, UsageModelRow, UsageSummary};
use crate::config::types::Config;
use crate::gateway::manager::{
    rebuild_if_running, start_gateway_inner, stop_gateway_inner, sync_consumer_models,
};
use crate::infra::db;
use crate::infra::error::IpcError;

#[tauri::command]
pub fn get_config(app: tauri::AppHandle) -> Config {
    let ctrl = app.state::<AppCtrl>();
    let mut cfg = ctrl.config.lock().unwrap().clone();
    sync_consumer_models(&mut cfg);
    cfg
}

#[tauri::command]
pub async fn save_config(app: tauri::AppHandle, mut cfg: Config) -> Result<(), IpcError> {
    sync_consumer_models(&mut cfg);
    {
        let ctrl = app.state::<AppCtrl>();
        let old_level = ctrl.config.lock().unwrap().log.level.clone();
        let conn = ctrl.db.lock().unwrap();
        db::save_config(&conn, &cfg)?;
        // 保存后重新从 DB 加载，确保新建的 provider 拿到正确的 id
        let reloaded = db::load_config(&conn)?;
        drop(conn);
        if reloaded.log.level != old_level {
            ctrl.log_level_setter.set(&reloaded.log.level);
        }
        *ctrl.config.lock().unwrap() = reloaded;
    }
    rebuild_if_running(&app).await?;
    Ok(())
}

#[tauri::command]
pub async fn start_gateway(app: tauri::AppHandle) -> Result<String, IpcError> {
    start_gateway_inner(&app).await
}

#[tauri::command]
pub async fn stop_gateway(app: tauri::AppHandle) -> Result<(), IpcError> {
    stop_gateway_inner(&app).await
}

#[tauri::command]
pub fn gateway_status(app: tauri::AppHandle) -> GatewayStatus {
    let ctrl = app.state::<AppCtrl>();
    let running = ctrl.server.lock().unwrap().is_some();
    let listen_addr = ctrl.listen_addr.lock().unwrap().clone();
    GatewayStatus {
        running,
        listen_addr,
    }
}

#[tauri::command]
pub async fn toggle_provider(
    app: tauri::AppHandle,
    name: String,
    enabled: bool,
) -> Result<(), IpcError> {
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
        db::save_config(&ctrl.db.lock().unwrap(), &cfg)?;
    }
    rebuild_if_running(&app).await?;
    tracing::info!(provider = %name, enabled, "provider 已切换");
    Ok(())
}

#[tauri::command]
pub async fn toggle_key(
    app: tauri::AppHandle,
    provider_name: String,
    key_idx: usize,
    enabled: bool,
) -> Result<(), IpcError> {
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
        db::save_config(&ctrl.db.lock().unwrap(), &cfg)?;
    }
    rebuild_if_running(&app).await?;
    tracing::info!(provider = %provider_name, key_idx, enabled, "key 已切换");
    Ok(())
}

#[tauri::command]
pub fn runtime_status(app: tauri::AppHandle) -> Vec<ProviderRuntime> {
    let ctrl = app.state::<AppCtrl>();
    let cfg = ctrl.config.lock().unwrap();
    let providers = ctrl.providers.lock().unwrap();

    cfg.providers
        .iter()
        .map(|pc| {
            let runtime_keys = providers
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
pub fn enable_autostart(app: tauri::AppHandle) -> Result<(), IpcError> {
    app.autolaunch()
        .enable()
        .map_err(|e| IpcError(e.to_string()))
}

#[tauri::command]
pub fn disable_autostart(app: tauri::AppHandle) -> Result<(), IpcError> {
    app.autolaunch()
        .disable()
        .map_err(|e| IpcError(e.to_string()))
}

#[tauri::command]
pub fn autostart_status(app: tauri::AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// 查询用量统计。consumer_key=None 查全部当前 key，days=None 查全部时间
#[tauri::command]
pub fn get_usage(
    app: tauri::AppHandle,
    consumer_key: Option<String>,
    days: Option<u32>,
) -> UsageSummary {
    let ctrl = app.state::<AppCtrl>();

    // 从当前 config 解析出实际的 consumer key 列表
    let consumer_keys: Vec<String> = match &consumer_key {
        None => ctrl.config.lock().unwrap().consumer.api_keys.clone(),
        Some(k) => vec![k.clone()],
    };

    let since = days
        .map(|d| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0) as i64;
            now - (d as i64 * 86400)
        })
        .unwrap_or(0);

    let conn = ctrl.db.lock().unwrap();
    let rows = db::query_usage(&conn, &consumer_keys, since).unwrap_or_default();
    let total_requests: u64 = rows.iter().map(|r| r.requests).sum();
    let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
    let total_output: u64 = rows.iter().map(|r| r.output_tokens).sum();
    let total_tokens: u64 = rows.iter().map(|r| r.total_tokens).sum();

    UsageSummary {
        models: rows
            .into_iter()
            .map(|r| UsageModelRow {
                model: r.model,
                requests: r.requests,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                total_tokens: r.total_tokens,
            })
            .collect(),
        total_requests,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_tokens,
    }
}

/// 查询供应商用量统计。
/// provider_id=None 查全部当前供应商；provider_key=None 查该供应商全部当前 key。
#[tauri::command]
pub fn get_provider_usage(
    app: tauri::AppHandle,
    provider_id: Option<i64>,
    provider_key: Option<String>,
    days: Option<u32>,
) -> UsageSummary {
    let ctrl = app.state::<AppCtrl>();

    // 从当前 config 解析出实际的 provider id 列表和 key 列表
    let (provider_ids, provider_keys) = {
        let cfg = ctrl.config.lock().unwrap();
        match provider_id {
            None => {
                // 全部供应商：收集所有当前 provider 的 id
                let ids: Vec<i64> = cfg.providers.iter().map(|p| p.id).collect();
                (ids, Vec::new())
            }
            Some(pid) => {
                // 指定供应商：收集该供应商的所有当前 key
                let keys: Vec<String> = cfg
                    .providers
                    .iter()
                    .find(|p| p.id == pid)
                    .map(|p| p.api_keys.iter().map(|k| k.key().to_string()).collect())
                    .unwrap_or_default();
                match provider_key {
                    None => (vec![pid], keys),
                    Some(k) => (vec![pid], vec![k]),
                }
            }
        }
    };

    let since = days
        .map(|d| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0) as i64;
            now - (d as i64 * 86400)
        })
        .unwrap_or(0);

    let conn = ctrl.db.lock().unwrap();
    let rows =
        db::query_provider_usage(&conn, &provider_ids, &provider_keys, since).unwrap_or_default();
    let total_requests: u64 = rows.iter().map(|r| r.requests).sum();
    let total_input: u64 = rows.iter().map(|r| r.input_tokens).sum();
    let total_output: u64 = rows.iter().map(|r| r.output_tokens).sum();
    let total_tokens: u64 = rows.iter().map(|r| r.total_tokens).sum();

    UsageSummary {
        models: rows
            .into_iter()
            .map(|r| UsageModelRow {
                model: r.model,
                requests: r.requests,
                input_tokens: r.input_tokens,
                output_tokens: r.output_tokens,
                total_tokens: r.total_tokens,
            })
            .collect(),
        total_requests,
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_tokens,
    }
}
