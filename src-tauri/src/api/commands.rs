use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;

use std::collections::HashMap;

use crate::config::state::{AppCtrl, GatewayStatus, ProviderRuntime, UsageModelRow, UsageSummary};
use crate::config::types::Config;
use crate::gateway::manager::{
    rebuild_if_running, start_gateway_inner, stop_gateway_inner, sync_consumer_models,
};
use crate::infra::db;
use crate::config::claude::{self as claude, CcForm, CcLoadResult};
use crate::config::codex::{self, CodexForm, CodexLoadResult, CodexSaveResult};
use crate::config::opencode::{self, OcForm, OcLoadResult};
use crate::error::IpcError;

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
        let db = ctrl.db.clone();
        // DB 事务（save + reload）可能耗时，放 spawn_blocking 避免阻塞 async runtime
        let reloaded = tauri::async_runtime::spawn_blocking(move || -> Result<Config, IpcError> {
            let conn = db.lock().map_err(|e| IpcError(format!("db lock: {e}")))?;
            db::save_config(&conn, &cfg)?;
            // 保存后重新从 DB 加载，确保新建的 provider 拿到正确的 id
            let reloaded = db::load_config(&conn)?;
            Ok(reloaded)
        })
        .await
        .map_err(|e| IpcError(format!("join: {e}")))??;
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

/// 由前端在**页面就绪后**调用：若 `auto_start_gateway` 且上次退出时网关在运行
/// （DB `gateway_running` == "1"），则启动网关。
///
/// 把「自动恢复网关」从启动早期（backend setup）推迟到 UI 就绪之后执行，
/// 避免网关先于界面就绪。返回是否实际启动了网关。
#[tauri::command]
pub async fn autostart_gateway_if_configured(app: tauri::AppHandle) -> Result<bool, IpcError> {
    let should = {
        let ctrl = app.state::<AppCtrl>();
        let auto = ctrl.config.lock().unwrap().auto_start_gateway;
        let last_running = db::get_setting(&ctrl.db.lock().unwrap(), "gateway_running")
            .map(|v| v == "1")
            .unwrap_or(false);
        auto && last_running
    };
    if !should {
        return Ok(false);
    }
    start_gateway_inner(&app).await?;
    Ok(true)
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
        // 找到目标 provider（找不到时返回错误，不再静默成功）
        let target = cfg
            .providers
            .iter_mut()
            .find(|p| p.name == name)
            .ok_or_else(|| IpcError(format!("provider 不存在: {name}")))?;
        target.enabled = enabled;
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
        let target = cfg
            .providers
            .iter_mut()
            .find(|p| p.name == provider_name)
            .ok_or_else(|| IpcError(format!("provider 不存在: {provider_name}")))?;
        // key_idx 越界校验（前端可能传错误值，或本地配置已变）
        if key_idx >= target.api_keys.len() {
            return Err(IpcError(format!(
                "key_idx {key_idx} 越界（provider {provider_name} 共 {} 个 key）",
                target.api_keys.len()
            )));
        }
        target.api_keys[key_idx].set_enabled(enabled);
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

/// 查询各别名上次成功响应的实际模型（内存记录，进程退出即失）。
/// 返回 别名 → 实际模型 的映射，供前端高亮当前命中的后端模型。
#[tauri::command]
pub fn last_used_models(app: tauri::AppHandle) -> HashMap<String, String> {
    let ctrl = app.state::<AppCtrl>();
    let map = ctrl.last_model.lock().unwrap().clone();
    map
}

/// 按自然日计算 since 时间戳。
/// days=None 查全部时间（返回 0）；days=Some(d) 返回本地今天 0 点起往前 d 个自然日（含今天）的时间戳。
fn since_for_days(days: Option<u32>) -> i64 {
    use chrono::{Local, TimeZone};
    match days {
        None => 0,
        Some(d) => {
            let today = Local::now().date_naive();
            // 含今天：d=1 → 今天 0 点；d=7 → 6 天前 0 点
            let start = today - chrono::Duration::days(d.saturating_sub(1) as i64);
            Local
                .from_local_datetime(&start.and_hms_opt(0, 0, 0).unwrap())
                .unwrap()
                .timestamp()
        }
    }
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

    let since = since_for_days(days);

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

    let since = since_for_days(days);

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

// ===================== OpenCode 配置编辑 =====================

/// 读取并解析 `~/.config/opencode/opencode.jsonc`，提取表单字段。
/// 文件不存在时返回空表单（exists=false），前端可据此新建。
#[tauri::command]
pub fn opencode_config_load() -> Result<OcLoadResult, IpcError> {
    opencode::load().map_err(|e| IpcError(format!("读取 opencode 配置失败: {e}")))
}

/// 把表单按 key 合并写回配置文件（仅覆盖 model / small_model / default_agent /
/// provider，其余字段原样保留；保存前自动备份 .bak）。
#[tauri::command]
pub fn opencode_config_save(form: OcForm) -> Result<(), IpcError> {
    opencode::save(&form).map_err(|e| IpcError(format!("保存 opencode 配置失败: {e}")))
}

/// 执行 `opencode models` 获取 opencode 当前可用的：
///   - provider id 列表（屏蔽下拉 disabled_providers 候选）
///   - 完整 `provider/model` 列表（主/轻量模型下拉候选）
/// 一次命令同时返回两份数据。
/// 用 spawn_blocking 包裹，避免阻塞 tokio worker（opencode 为外部进程）。
#[tauri::command]
pub async fn opencode_models_catalog() -> Result<opencode::ModelsCatalog, IpcError> {
    tauri::async_runtime::spawn_blocking(|| {
        opencode::list_models_catalog()
            .map_err(|e| IpcError(format!("获取 opencode 模型目录失败: {e}")))
    })
    .await
    .map_err(|e| IpcError(format!("任务调度失败: {e}")))?
}

/// 执行 `opencode -v` 获取版本号；未安装返回 None。
/// 前端据此决定是否显示「OpenCode 配置」侧边栏入口。
#[tauri::command]
pub async fn opencode_version() -> Result<Option<String>, IpcError> {
    tauri::async_runtime::spawn_blocking(|| Ok(opencode::version()))
        .await
        .map_err(|e| IpcError(format!("任务调度失败: {e}")))?
}

// ===================== Claude Code 配置编辑 =====================

/// 读取并解析 `~/.claude/settings.json`（或 `$CLAUDE_CONFIG_DIR/settings.json`），
/// 提取 `env` 段。文件不存在时返回空表单（exists=false），前端可据此新建。
#[tauri::command]
pub fn claude_code_config_load() -> Result<CcLoadResult, IpcError> {
    claude::load().map_err(|e| IpcError(format!("读取 claude code 配置失败: {e}")))
}

/// 把表单的 env 段整体合并写回配置文件（仅替换 `env` key，
/// 其余顶层字段如 enabledPlugins / statusLine 原样保留；保存前自动备份 .bak）。
#[tauri::command]
pub fn claude_code_config_save(form: CcForm) -> Result<(), IpcError> {
    claude::save(&form).map_err(|e| IpcError(format!("保存 claude code 配置失败: {e}")))
}

/// 执行 `claude --version` 获取版本号；未安装返回 None。
/// 前端据此决定是否显示「Claude Code 配置」侧边栏入口。
/// 用 spawn_blocking 包裹，避免阻塞 tokio worker（claude 为外部进程）。
#[tauri::command]
pub async fn claude_code_version() -> Result<Option<String>, IpcError> {
    tauri::async_runtime::spawn_blocking(|| Ok(claude::version()))
        .await
        .map_err(|e| IpcError(format!("任务调度失败: {e}")))?
}

// ===================== Codex 配置编辑 =====================

/// 读取并解析 `~/.codex/config.toml`（或 `$CODEX_HOME/config.toml`），
/// 提取受管字段（顶层 model / model_provider + 受管 [model_providers.<id>]）。
/// 文件不存在时返回默认空壳（exists=false），前端可据此新建。
#[tauri::command]
pub fn codex_config_load() -> Result<CodexLoadResult, IpcError> {
    codex::load().map_err(|e| IpcError(format!("读取 codex 配置失败: {e}")))
}

/// 把表单按 key 合并写回配置文件（仅覆盖受管字段，其余字段原样保留；
/// 受管 provider 改名时清理旧表；保存前自动备份 .bak）。
///
/// 若开启模型目录且清单非空，先克隆内置模板生成 `ai-aggregs.catalog.json`，
/// 再把其路径写入 `model_catalog_json`；生成失败则不设该 key（返回错误供前端提示）。
#[tauri::command]
pub fn codex_config_save(form: CodexForm) -> Result<CodexSaveResult, IpcError> {
    // 模型目录：开启且清单非空 → 生成 catalog 文件；否则不设 model_catalog_json
    let (catalog_path, result) = if form.enable_model_catalog {
        if form.catalog_models.iter().any(|s| !s.trim().is_empty()) {
            match codex::generate_catalog(&form.catalog_models) {
                Ok((path, count)) => (
                    Some(path),
                    CodexSaveResult {
                        catalog_ok: true,
                        catalog_count: count,
                        catalog_error: None,
                    },
                ),
                Err(e) => (
                    None,
                    CodexSaveResult {
                        catalog_ok: false,
                        catalog_count: 0,
                        catalog_error: Some(e.to_string()),
                    },
                ),
            }
        } else {
            (
                None,
                CodexSaveResult {
                    catalog_ok: false,
                    catalog_count: 0,
                    catalog_error: Some("模型清单为空".into()),
                },
            )
        }
    } else {
        (
            None,
            CodexSaveResult {
                catalog_ok: false,
                catalog_count: 0,
                catalog_error: None,
            },
        )
    };
    codex::save(&form, catalog_path.as_deref())
        .map_err(|e| IpcError(format!("保存 codex 配置失败: {e}")))?;
    Ok(result)
}

/// 执行 `codex --version` 获取版本号；未安装返回 None。
/// 前端据此决定是否显示「Codex 配置」侧边栏入口。
/// 用 spawn_blocking 包裹，避免阻塞 tokio worker（codex 为外部进程）。
#[tauri::command]
pub async fn codex_version() -> Result<Option<String>, IpcError> {
    tauri::async_runtime::spawn_blocking(|| Ok(codex::version()))
        .await
        .map_err(|e| IpcError(format!("任务调度失败: {e}")))?
}

/// 获取网关运行时 metrics 快照（协议转换次数、流式活跃数、上游错误分类等）。
///
/// 用于前端「网关状态」面板展示可观测性数据。零运行时开销（AtomicU64 读取）。
#[tauri::command]
pub fn gateway_metrics(app: tauri::AppHandle) -> crate::observability::MetricsSnapshot {
    let ctrl = app.state::<AppCtrl>();
    ctrl.metrics.snapshot()
}

