use tauri::Manager;
use tauri_plugin_autostart::ManagerExt;

use std::collections::HashMap;

use crate::config::state::{AppCtrl, GatewayStatus, ProviderRuntime, UsageModelRow, UsageSummary};
use crate::config::types::{Config, Protocol};
use std::time::Duration;
use crate::gateway::manager::{
    rebuild_if_running, start_gateway_inner, stop_gateway_inner, sync_consumer_models,
};
use crate::infra::db;
use crate::infra::error::IpcError;
use crate::infra::opencode::{self, OcForm, OcLoadResult};
use crate::infra::claude_code::{self, CcForm, CcLoadResult};
use crate::infra::codex::{self, CodexForm, CodexLoadResult, CodexSaveResult};

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
    claude_code::load().map_err(|e| IpcError(format!("读取 claude code 配置失败: {e}")))
}

/// 把表单的 env 段整体合并写回配置文件（仅替换 `env` key，
/// 其余顶层字段如 enabledPlugins / statusLine 原样保留；保存前自动备份 .bak）。
#[tauri::command]
pub fn claude_code_config_save(form: CcForm) -> Result<(), IpcError> {
    claude_code::save(&form).map_err(|e| IpcError(format!("保存 claude code 配置失败: {e}")))
}

/// 执行 `claude --version` 获取版本号；未安装返回 None。
/// 前端据此决定是否显示「Claude Code 配置」侧边栏入口。
/// 用 spawn_blocking 包裹，避免阻塞 tokio worker（claude 为外部进程）。
#[tauri::command]
pub async fn claude_code_version() -> Result<Option<String>, IpcError> {
    tauri::async_runtime::spawn_blocking(|| Ok(claude_code::version()))
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

/// 从上游供应商的 `/models` 端点拉取其支持的全部模型列表。
///
/// 用于「供应商编辑」弹窗：用户点击模型输入框时自动触发，把上游支持的全部
/// 模型填入下拉候选，避免手工逐个输入模型名。
///
/// - URL 拼接：`{base_url}/models`（与网关转发请求 `base_url + endpoint()` 一致，
///   base_url 的约定由用户保证可拼接，如 `https://api.openai.com/v1`）。
/// - 鉴权头复用 gateway 的 `adapter_for(protocol).auth_headers(key)`，与聊天转发
///   完全一致：chat / responses → `Authorization: Bearer {key}`，anthropic →
///   `x-api-key: {key}` + `anthropic-version: 2023-06-01`。
/// - 伪装浏览器 User-Agent：部分上游（Cloudflare 防护）会按 UA 屏蔽非浏览器客户端。
/// - 响应解析：兼容 OpenAI / Anthropic / 第三方代理的常见格式，
///   从 `data[].id` / `data[].name` / `models[].id` / `models[].name` 提取模型名。
///
/// 直接用一次性 reqwest::Client，不复用运行中 Provider 的私有 client 字段，
/// 因为编辑模态框里的 provider 可能尚未入库（新建场景），没有 Provider 实例可复用。
#[tauri::command]
pub async fn fetch_provider_models(
    base_url: String,
    api_key: String,
    protocol: Protocol,
) -> Result<Vec<String>, IpcError> {
    // 参数校验：base_url / api_key 不能为空（前端也会校验，这里兜底）
    let base = base_url.trim();
    let key = api_key.trim();
    if base.is_empty() || key.is_empty() {
        return Err(IpcError("base_url 与 api_key 不能为空".into()));
    }
    let base = base.trim_end_matches('/');
    let url = format!("{base}/models");

    // 候选拉取是非关键路径（仅用于下拉补全），用较短超时让不可达上游快速失败
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| IpcError(format!("构造 HTTP 客户端失败: {e}")))?;

    // 复用 gateway 的协议适配器构造鉴权头，确保与聊天转发完全一致
    let adapter = crate::gateway::provider::adapter_for(protocol);
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, value) in adapter.auth_headers(key) {
        headers.insert(name, value);
    }
    // 伪装浏览器 User-Agent：部分上游（如 Cloudflare 防护的站点）会按 UA 屏蔽
    // 非浏览器客户端（reqwest 默认无 UA 或为 `reqwest/x.y.z` 会直接被拦截连不上）。
    // 浏览器 UA 能让请求通过 CDN 的简单 bot 检测，不影响真正的 API 鉴权。
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
    if let Ok(v) = reqwest::header::HeaderValue::from_str(ua) {
        headers.insert(reqwest::header::USER_AGENT, v);
    }

    tracing::debug!(url = %url, protocol = %protocol.as_str(), "→ 拉取上游模型列表");
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(8))
        .headers(headers)
        .send()
        .await
        .map_err(|e| {
            // 连接/超时/TLS 错误也记录日志，便于从 logs/ 排查（前端静默无提示）
            tracing::warn!(
                url = %url, err = %e,
                is_timeout = e.is_timeout(), is_connect = e.is_connect(),
                "拉取上游模型列表连接失败"
            );
            IpcError(format!("请求上游失败: {e}"))
        })?;
    let status = resp.status();
    if !status.is_success() {
        // 上游非 2xx：把响应文本带回来便于排查（如 key 无效、路径错误）
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!(url = %url, status = %status, body = %text, "拉取上游模型列表失败");
        return Err(IpcError(format!("上游返回 {status}: {text}")));
    }

    // 解析响应：兼容多种格式
    //   OpenAI / Anthropic:  { data: [{ id: "..." }] }
    //   部分第三方代理:       { data: [{ name: "..." }] }  或  { models: [{ id }] }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| IpcError(format!("解析响应 JSON 失败: {e}")))?;

    /// 从一个 JSON 数组中提取模型名字段（尝试 id → name → model 顺序）
    fn extract_ids(arr: &[serde_json::Value]) -> Vec<String> {
        arr.iter()
            .filter_map(|m| {
                // 纯字符串数组（某些代理直接返回 ["model-a", "model-b"]）
                if let Some(s) = m.as_str() {
                    return if s.is_empty() { None } else { Some(s.to_string()) };
                }
                // 对象数组：按优先级尝试 id / name / model 字段
                m.get("id")
                    .or_else(|| m.get("name"))
                    .or_else(|| m.get("model"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .collect()
    }

    let models = body
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| body.get("models").and_then(|m| m.as_array()))
        .map(|arr: &Vec<serde_json::Value>| extract_ids(arr))
        .unwrap_or_default();

    tracing::info!(url = %url, count = models.len(), "拉取上游模型列表成功");
    Ok(models)
}

