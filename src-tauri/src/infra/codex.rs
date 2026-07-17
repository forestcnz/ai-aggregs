//! Codex（OpenAI Codex CLI）配置（`~/.codex/config.toml`）读写与网关联动。
//!
//! 仅管理 `config.toml` 的少量字段，把 Codex 指向本网关：
//!   - 顶层 `model`、`model_provider`（= 受管 provider 的 id）
//!   - `[model_providers.<id>]` 的 `name` / `base_url` / `experimental_bearer_token`
//!
//! Codex 现仅支持 `wire_api = "responses"`（且为默认值），故不写入 `wire_api`；
//! 网关以 Responses 协议端点（`/v1/responses`）对接。鉴权用 `experimental_bearer_token`
//! 直接写入网关 consumer key（开箱即用；与 `env_key`/`requires_openai_auth` 互斥，故不写后者）。
//!
//! 仅管理**一条**受管 provider（默认 id `aggregs`）。保存时按 key 合并：只覆盖上述受管
//! 字段，其它顶层键、其它 `[model_providers.*]` 表、以及受管表内的其它键（如
//! `http_headers`/`query_params`）一律原样保留。受管 provider 改名时清理旧表。
//!
//! 注意：TOML `#` 注释在 `toml::to_string` 重新序列化后会丢失。保存前自动备份上一份，
//! 文件名带时间戳：`config.toml.YYYYMMDD_HHMMSS.bak`；并通过 `has_comments` 提示前端。
//!
//! 路径定位：优先 `CODEX_HOME` 环境变量，否则 `$HOME/.codex/config.toml`
//! （`HOME` → `USERPROFILE`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

// ===================== 路径 =====================

/// Codex 用户级目录：优先 `CODEX_HOME`，否则 `$HOME/.codex`（`HOME` → `USERPROFILE`）。
fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
                .unwrap_or_default();
            if home.is_empty() {
                None
            } else {
                Some(PathBuf::from(home).join(".codex"))
            }
        })
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

/// 返回 Codex 配置文件路径 `config.toml`。返回期望路径，不强建目录。
pub fn config_path() -> PathBuf {
    codex_home().join("config.toml")
}

/// 本应用生成的模型目录文件路径（`ai-aggregs.catalog.json`）。
/// 作为 `model_catalog_json` 的指向，也是手动模型清单的持久化载体。
pub fn catalog_path() -> PathBuf {
    codex_home().join("ai-aggregs.catalog.json")
}

// ===================== TOML 注释检测 =====================

/// 检测 TOML 文本是否含 `#` 注释（尊重基本字符串 `"..."` 与字面字符串 `'...'`，
/// 多行字符串内的 `#` 极少见，偶尔误判可接受——仅用于「注释将丢失」提示）。
pub fn has_comments(raw: &str) -> bool {
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        // 基本字符串：跳到闭合的未转义 "
        if c == '"' {
            i += 1;
            while i < n {
                if chars[i] == '\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        // 字面字符串：跳到闭合的 '
        if c == '\'' {
            i += 1;
            while i < n {
                if chars[i] == '\'' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '#' {
            return true;
        }
        i += 1;
    }
    false
}

// ===================== 表单数据结构 =====================

/// 受管 provider（指向本网关的那一条）。id 同时作为顶层 `model_provider` 的值。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CodexProvider {
    /// provider id（如 `aggregs`），即 `model_providers.<id>` 的 key 与 `model_provider` 的值
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 网关 consumer key 直接写入（开箱即用；与 env_key/requires_openai_auth 互斥）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_bearer_token: Option<String>,
}

/// 表单管理的 Codex 配置子集。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CodexForm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: CodexProvider,
    /// 加载时的受管 provider id（前端原样回传、不可编辑）。
    /// 保存时若与 `provider.id` 不同，则删除旧 `[model_providers.<旧id>]`（改名清理）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_provider_id: Option<String>,
    /// 是否在 config.toml 设 `model_catalog_json`（启用 /model 模型目录）。
    /// form-only，不写入 config.toml；由 config.toml 是否已含该 key 推断。
    #[serde(default)]
    pub enable_model_catalog: bool,
    /// 模型目录的手动清单（form-only；持久化于 catalog 文件，load 时回填）。
    /// 保存时若开启目录，则克隆内置模板为每个模型生成一个 catalog 条目。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_models: Vec<String>,
}

/// 受管 provider 的默认空壳（id = `aggregs`）。
fn default_provider() -> CodexProvider {
    CodexProvider {
        id: "aggregs".into(),
        name: None,
        base_url: None,
        experimental_bearer_token: None,
    }
}

// ===================== 表单 ↔ toml::Value 转换 =====================

/// Codex 内置 provider id（保留名，不可作为自定义 provider 覆盖）。
const RESERVED_IDS: &[&str] = &["openai", "ollama", "lmstudio"];

/// 从解析后的完整 toml::Value 中提取表单字段（忽略未知字段）。
///
/// 受管 provider 取「顶层 `model_provider` 指向、且非保留 id、且存在对应表」的那一条；
/// 否则种入默认空壳（id = `aggregs`），`loaded_provider_id = None`（保存时无需清理旧表）。
pub fn extract_form(root: &toml::Value) -> CodexForm {
    let Some(tbl) = root.as_table() else {
        return CodexForm {
            model: None,
            provider: default_provider(),
            loaded_provider_id: None,
            enable_model_catalog: false,
            catalog_models: Vec::new(),
        };
    };
    let model = tbl.get("model").and_then(toml::Value::as_str).map(String::from);
    let model_provider = tbl.get("model_provider").and_then(toml::Value::as_str).map(String::from);

    let (provider, loaded_provider_id) = match model_provider.as_deref() {
        Some(id)
            if !id.trim().is_empty() && !RESERVED_IDS.contains(&id.trim()) =>
        {
            let pv = tbl
                .get("model_providers")
                .and_then(toml::Value::as_table)
                .and_then(|t| t.get(id));
            let p = match pv {
                Some(v) => extract_provider(id, v),
                None => CodexProvider {
                    id: id.to_string(),
                    name: None,
                    base_url: None,
                    experimental_bearer_token: None,
                },
            };
            (p, Some(id.to_string()))
        }
        _ => (default_provider(), None),
    };
    let enable_model_catalog = tbl.get("model_catalog_json").is_some();
    CodexForm {
        model,
        provider,
        loaded_provider_id,
        enable_model_catalog,
        catalog_models: Vec::new(),
    }
}

fn extract_provider(id: &str, pv: &toml::Value) -> CodexProvider {
    let t = pv.as_table();
    let get_str = |key: &str| {
        t.and_then(|o| o.get(key))
            .and_then(toml::Value::as_str)
            .map(String::from)
    };
    CodexProvider {
        id: id.to_string(),
        name: get_str("name"),
        base_url: get_str("base_url"),
        experimental_bearer_token: get_str("experimental_bearer_token"),
    }
}

/// 在 table 上设置/删除一个字符串字段：非空（trim 后）覆盖，空则删除（clear 语义）。
fn set_str(tbl: &mut toml::value::Table, key: &str, val: Option<&str>) {
    match val.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            tbl.insert(key.into(), toml::Value::String(s.to_string()));
        }
        None => {
            tbl.remove(key);
        }
    }
}

/// 把表单管理的字段合并进磁盘读入的 toml::Value。
///
/// 仅动：顶层 `model`、`model_provider`、`[model_providers.<provider.id>]` 的
/// `name`/`base_url`/`experimental_bearer_token`。其它一律不动。
/// 受管表内已存在的其它键（如 `http_headers`）保留；改名时清理旧表。
pub fn merge_form(root: &mut toml::Value, form: &CodexForm, catalog_path: Option<&str>) {
    let Some(tbl) = root.as_table_mut() else {
        return;
    };
    let new_id = form.provider.id.trim();

    // 1. 改名清理：删除旧受管表
    if let Some(old) = form.loaded_provider_id.as_deref().map(str::trim) {
        if !old.is_empty() && old != new_id {
            if let Some(mp) = tbl
                .get_mut("model_providers")
                .and_then(toml::Value::as_table_mut)
            {
                mp.remove(old);
            }
        }
    }

    // 2. 写受管 provider 表（合并，保留表内其它键）
    if !new_id.is_empty() {
        if !tbl.contains_key("model_providers") {
            tbl.insert(
                "model_providers".into(),
                toml::Value::Table(toml::value::Table::new()),
            );
        }
        if let Some(mp) = tbl
            .get_mut("model_providers")
            .and_then(toml::Value::as_table_mut)
        {
            let entry = mp
                .entry(new_id.to_string())
                .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
            if let Some(ptbl) = entry.as_table_mut() {
                set_str(ptbl, "name", form.provider.name.as_deref());
                set_str(ptbl, "base_url", form.provider.base_url.as_deref());
                set_str(
                    ptbl,
                    "experimental_bearer_token",
                    form.provider.experimental_bearer_token.as_deref(),
                );
            }
            // 受管表若被清空（三字段皆空）则移除，避免留空 [model_providers.<id>]
            if let Some(t) = mp.get(new_id).and_then(toml::Value::as_table) {
                if t.is_empty() {
                    mp.remove(new_id);
                }
            }
        }
    }

    // 3. 顶层 model
    set_str(tbl, "model", form.model.as_deref());

    // 4. 顶层 model_provider：始终激活受管 provider（本页用途即「让 Codex 用本网关」）
    if new_id.is_empty() {
        tbl.remove("model_provider");
    } else {
        tbl.insert(
            "model_provider".into(),
            toml::Value::String(new_id.to_string()),
        );
    }

    // 5. model_catalog_json（受管：Some=设路径启用 /model 目录，None=移除 key 恢复内置列表）
    match catalog_path.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => {
            tbl.insert("model_catalog_json".into(), toml::Value::String(p.to_string()));
        }
        None => {
            tbl.remove("model_catalog_json");
        }
    }
}

// ===================== 文件读写 =====================

/// load 返回的复合结构。
#[derive(Serialize)]
pub struct CodexLoadResult {
    /// 解析后的配置文件绝对路径
    pub path: String,
    /// 文件是否存在（不存在时 form 为默认空壳）
    pub exists: bool,
    /// 原文件是否含 `#` 注释（前端据此提示「注释将丢失」）
    pub has_comments: bool,
    /// 表单数据
    pub form: CodexForm,
}

/// save 返回的复合结构（前端据此 toast 模型目录生成结果）。
#[derive(Serialize, Debug)]
pub struct CodexSaveResult {
    /// 模型目录是否成功生成并写入
    pub catalog_ok: bool,
    /// 生成的 catalog 条目数（0 = 未启用或失败）
    pub catalog_count: usize,
    /// 目录未生成时的原因（未启用时为 None）
    pub catalog_error: Option<String>,
}

/// 读取并解析 Codex 配置文件，提取表单字段。文件不存在时返回默认空壳（exists=false）。
pub fn load() -> std::io::Result<CodexLoadResult> {
    let path = config_path();
    let path_str = path.to_string_lossy().to_string();
    if !path.exists() {
        return Ok(CodexLoadResult {
            path: path_str,
            exists: false,
            has_comments: false,
            form: CodexForm {
                model: None,
                provider: default_provider(),
                loaded_provider_id: None,
                ..Default::default()
            },
        });
    }
    let raw = std::fs::read_to_string(&path)?;
    let has_comments = has_comments(&raw);
    // 解析失败时退回空表，避免阻塞 UI（文件可能被外部写成非法 TOML）
    let root: toml::Value =
        toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(toml::value::Table::new()));
    let mut form = extract_form(&root);
    // 从本应用生成的 catalog 文件回填手动模型清单（跨会话不丢）
    form.catalog_models = read_catalog_slugs();
    Ok(CodexLoadResult {
        path: path_str,
        exists: true,
        has_comments,
        form,
    })
}

/// 把表单按 key 合并写回配置文件。
/// - 先读取磁盘当前内容（保留外部可能的修改与未管理字段）
/// - 备份原文件为 `.bak`
/// - 合并受管字段后以 `toml::to_string_pretty` 写回
///
/// 文件不存在时直接以表单字段新建。
pub fn save(form: &CodexForm, catalog_path: Option<&str>) -> std::io::Result<()> {
    let path = config_path();
    let mut root: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str(&raw).unwrap_or_else(|_| toml::Value::Table(toml::value::Table::new()))
    } else {
        toml::Value::Table(toml::value::Table::new())
    };
    if !root.is_table() {
        root = toml::Value::Table(toml::value::Table::new());
    }
    merge_form(&mut root, form, catalog_path);

    // 备份原文件（存在时）—— 带时间戳，避免多次保存互相覆盖
    if path.exists() {
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let bak_name = format!(
            "{}.{}.bak",
            path.file_name().unwrap_or_default().to_string_lossy(),
            ts
        );
        let bak_path = path
            .parent()
            .map(|p| p.join(&bak_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&bak_name));
        if let Err(e) = std::fs::copy(&path, &bak_path) {
            tracing::warn!(err = %e, "备份 codex 配置失败，继续写入");
        }
    }

    // 确保父目录存在
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let pretty = toml::to_string_pretty(&root)
        .map_err(|e| std::io::Error::other(format!("序列化失败: {e}")))?;
    std::fs::write(&path, pretty)?;
    Ok(())
}

// ===================== 模型目录（model_catalog_json） =====================

/// 读取本应用生成的 catalog 文件，提取各 entry 的 slug 列表（用于回填手动清单）。
/// 文件缺失/解析失败时返回空（不报错——目录可能尚未生成）。
fn read_catalog_slugs() -> Vec<String> {
    let path = catalog_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let v: JsonValue = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    v.get("models")
        .and_then(JsonValue::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("slug").and_then(JsonValue::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 克隆模板 entry 为每个模型名生成一条 catalog 条目，仅覆盖标识字段
/// （slug/display_name/description/visibility）；其余 required 字段（含 base_instructions）
/// 沿用模板，跨 Codex 版本天然完整。
fn build_catalog_entries(template: &JsonValue, names: &[String]) -> Vec<JsonValue> {
    names
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|name| {
            let mut e = template.clone();
            if let Some(obj) = e.as_object_mut() {
                obj.insert("slug".into(), json!(name));
                obj.insert("display_name".into(), json!(name));
                obj.insert("description".into(), json!("via AI 聚合网关"));
                obj.insert("visibility".into(), json!("list"));
            }
            e
        })
        .collect()
}

/// 生成模型目录文件：执行 `codex debug models --bundled` 取内置 catalog 作模板，
/// 克隆出 `model_names` 对应的条目，写入 `ai-aggregs.catalog.json`。
/// 返回（文件绝对路径, 条目数）。
///
/// **稳健性**：克隆内置模板而非手写，规避 Codex 跨版本 required 字段差异。
/// **降级**：bundled 取不到/无模板/清单空 → 返回 Err（调用方据此不设 model_catalog_json）。
pub fn generate_catalog(model_names: &[String]) -> std::io::Result<(String, usize)> {
    let output = run_codex("debug models --bundled")?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "codex debug models --bundled 执行失败（退出码 {:?}）",
            output.status.code()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let bundled: JsonValue = serde_json::from_str(&stdout)
        .map_err(|e| std::io::Error::other(format!("解析 bundled catalog 失败: {e}")))?;
    // 防御式定位模板：先 .models 数组，再退顶级数组
    let template = bundled
        .get("models")
        .and_then(JsonValue::as_array)
        .and_then(|a| a.first())
        .or_else(|| bundled.as_array().and_then(|a| a.first()))
        .ok_or_else(|| std::io::Error::other("bundled catalog 无模型条目，无法克隆模板"))?
        .clone();
    if !template.is_object() {
        return Err(std::io::Error::other("bundled 模板非对象"));
    }

    let entries = build_catalog_entries(&template, model_names);
    if entries.is_empty() {
        return Err(std::io::Error::other("模型清单为空"));
    }
    let count = entries.len();

    let catalog = json!({ "models": entries });
    let path = catalog_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let pretty = serde_json::to_string_pretty(&catalog)
        .map_err(|e| std::io::Error::other(format!("序列化 catalog 失败: {e}")))?;
    std::fs::write(&path, pretty)?;
    Ok((path.to_string_lossy().to_string(), count))
}

// ===================== 执行 codex 命令 =====================

/// 通过系统 shell 执行 `codex <args>`，由 shell 负责按 PATH / PATHEXT 解析
/// （Windows 上 codex 是 npm 生成的 .cmd shim，需经 cmd.exe；Unix 经 sh）。
fn run_codex(args: &str) -> std::io::Result<std::process::Output> {
    let (shell, flag) = if cfg!(target_os = "windows") {
        ("cmd.exe", "/c")
    } else {
        ("sh", "-c")
    };
    std::process::Command::new(shell)
        .arg(flag)
        .arg(format!("codex {args}"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
}

/// 执行 `codex --version` 获取版本号；未安装或执行失败时返回 None。
/// 前端据此决定是否显示「Codex 配置」侧边栏入口。
/// 取整行 trimmed（Codex 输出可能带 `codex-cli` 前缀，取首 token 会丢版本号）。
pub fn version() -> Option<String> {
    let output = run_codex("--version").ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_reads_managed_provider() {
        let toml_src = r#"
model = "gpt-4o"
model_provider = "aggregs"

[model_providers.aggregs]
name = "AI 聚合网关"
base_url = "http://127.0.0.1:8000/v1"
experimental_bearer_token = "sk-con...0001"

[mcp_servers.foo]
command = "bar"
"#;
        let root: toml::Value = toml::from_str(toml_src).unwrap();
        let form = extract_form(&root);
        assert_eq!(form.model.as_deref(), Some("gpt-4o"));
        assert_eq!(form.provider.id, "aggregs");
        assert_eq!(
            form.provider.base_url.as_deref(),
            Some("http://127.0.0.1:8000/v1")
        );
        assert_eq!(
            form.provider.experimental_bearer_token.as_deref(),
            Some("sk-con...0001")
        );
        assert_eq!(form.loaded_provider_id.as_deref(), Some("aggregs"));
    }

    #[test]
    fn extract_seeds_default_when_model_provider_reserved() {
        // model_provider 指向内置 openai → 不应接管该表，种入默认 aggregs
        let toml_src = r#"
model = "gpt-4o"
model_provider = "openai"
"#;
        let root: toml::Value = toml::from_str(toml_src).unwrap();
        let form = extract_form(&root);
        assert_eq!(form.provider.id, "aggregs");
        assert_eq!(form.model.as_deref(), Some("gpt-4o"));
        assert!(form.loaded_provider_id.is_none());
    }

    #[test]
    fn merge_preserves_unmanaged_keys_and_sibling_tables() {
        let toml_src = r#"
model = "old"
model_provider = "openai"

[model_providers.openrouter]
name = "OpenRouter"

[model_providers.aggregs]
name = "旧名"
http_headers = { X-Foo = "bar" }

[mcp_servers.foo]
command = "bar"
"#;
        let mut root: toml::Value = toml::from_str(toml_src).unwrap();
        let form = CodexForm {
            model: Some("gpt-4o".into()),
            provider: CodexProvider {
                id: "aggregs".into(),
                name: Some("AI 聚合网关".into()),
                base_url: Some("http://127.0.0.1:8000/v1".into()),
                experimental_bearer_token: Some("sk-con".into()),
            },
            loaded_provider_id: Some("aggregs".into()),
            ..Default::default()
        };
        merge_form(&mut root, &form, None);
        let tbl = root.as_table().unwrap();
        // model / model_provider 更新
        assert_eq!(tbl.get("model").and_then(toml::Value::as_str), Some("gpt-4o"));
        assert_eq!(
            tbl.get("model_provider").and_then(toml::Value::as_str),
            Some("aggregs")
        );
        let mp = tbl
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .unwrap();
        // 兄弟表 openrouter 保留
        assert!(mp.contains_key("openrouter"));
        // 受管表内非受管键 http_headers 保留，受管键已更新
        let agg = mp.get("aggregs").and_then(toml::Value::as_table).unwrap();
        assert_eq!(
            agg.get("name").and_then(toml::Value::as_str),
            Some("AI 聚合网关")
        );
        assert!(agg.contains_key("http_headers"));
        // 顶层未管理键 mcp_servers 保留
        assert!(tbl.contains_key("mcp_servers"));
    }

    #[test]
    fn merge_renamed_provider_cleans_old_table() {
        let toml_src = r#"
model_provider = "aggregs"
[model_providers.aggregs]
base_url = "http://x/v1"
"#;
        let mut root: toml::Value = toml::from_str(toml_src).unwrap();
        let form = CodexForm {
            model: None,
            provider: CodexProvider {
                id: "mygw".into(),
                name: None,
                base_url: Some("http://127.0.0.1:8000/v1".into()),
                experimental_bearer_token: Some("sk".into()),
            },
            loaded_provider_id: Some("aggregs".into()),
            ..Default::default()
        };
        merge_form(&mut root, &form, None);
        let mp = root
            .as_table()
            .unwrap()
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .unwrap();
        assert!(!mp.contains_key("aggregs"), "旧表应被清理");
        assert!(mp.contains_key("mygw"));
        assert_eq!(
            root.as_table()
                .unwrap()
                .get("model_provider")
                .and_then(toml::Value::as_str),
            Some("mygw")
        );
    }

    #[test]
    fn merge_empty_fields_drop_keys() {
        let toml_src = r#"
model = "gpt-4o"
model_provider = "aggregs"
[model_providers.aggregs]
name = "x"
"#;
        let mut root: toml::Value = toml::from_str(toml_src).unwrap();
        let form = CodexForm {
            model: Some("   ".into()),
            provider: CodexProvider {
                id: "aggregs".into(),
                name: None,
                base_url: None,
                experimental_bearer_token: None,
            },
            loaded_provider_id: Some("aggregs".into()),
            ..Default::default()
        };
        merge_form(&mut root, &form, None);
        let tbl = root.as_table().unwrap();
        assert!(tbl.get("model").is_none(), "空 model 应删除");
        // 受管表三字段皆空 → 整表移除
        let mp = tbl
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .unwrap();
        assert!(!mp.contains_key("aggregs"), "空受管表应移除");
    }

    #[test]
    fn has_comments_detects_hash_outside_strings() {
        assert!(has_comments("model = \"x\" # 主模型"));
        assert!(has_comments("# 整行注释\nmodel = \"y\""));
        assert!(!has_comments("base_url = \"http://x/v1#frag\""));
        assert!(!has_comments("model = 'a#b'"));
    }

    #[test]
    fn roundtrip_writes_expected_toml() {
        // 空文件 → 保存 → 文本应含受管字段，且 model_provider=aggregs
        let mut root = toml::Value::Table(toml::value::Table::new());
        let form = CodexForm {
            model: Some("gpt-4o".into()),
            provider: CodexProvider {
                id: "aggregs".into(),
                name: Some("AI 聚合网关".into()),
                base_url: Some("http://127.0.0.1:8000/v1".into()),
                experimental_bearer_token: Some("sk-con".into()),
            },
            loaded_provider_id: None,
            ..Default::default()
        };
        merge_form(&mut root, &form, None);
        let out = toml::to_string_pretty(&root).unwrap();
        assert!(out.contains("model = \"gpt-4o\""));
        assert!(out.contains("model_provider = \"aggregs\""));
        assert!(out.contains("[model_providers.aggregs]"));
        assert!(out.contains("base_url = \"http://127.0.0.1:8000/v1\""));
        assert!(out.contains("experimental_bearer_token = \"sk-con\""));
    }

    #[test]
    fn merge_enable_catalog_sets_key() {
        let mut root = toml::Value::Table(toml::value::Table::new());
        let form = CodexForm::default();
        merge_form(&mut root, &form, Some("/tmp/aggregs.catalog.json"));
        assert_eq!(
            root.as_table()
                .unwrap()
                .get("model_catalog_json")
                .and_then(toml::Value::as_str),
            Some("/tmp/aggregs.catalog.json")
        );
    }

    #[test]
    fn merge_disable_catalog_removes_key() {
        let mut root: toml::Value =
            toml::from_str(r#"model_catalog_json = "/old/catalog.json""#).unwrap();
        let form = CodexForm::default();
        merge_form(&mut root, &form, None);
        assert!(
            root.as_table()
                .unwrap()
                .get("model_catalog_json")
                .is_none(),
            "None 应移除 model_catalog_json"
        );
    }

    #[test]
    fn build_catalog_entries_overrides_slug() {
        // 模板含 base_instructions 等字段，克隆后只覆盖 slug/display_name/description/visibility
        let template: JsonValue = serde_json::from_str(
            r#"{"slug":"template","display_name":"T","description":"d","visibility":"hidden",
                "base_instructions":"full prompt","availability_nux":true,"upgrade":{}}"#,
        )
        .unwrap();
        let entries = build_catalog_entries(
            &template,
            &["glm-4-plus".into(), "  ".into(), "claude-3".into()],
        );
        assert_eq!(entries.len(), 2, "空白名应被过滤");
        let slugs: Vec<&str> = entries
            .iter()
            .filter_map(|e| e.get("slug").and_then(JsonValue::as_str))
            .collect();
        assert_eq!(slugs, vec!["glm-4-plus", "claude-3"]);
        // 模板原有字段保留
        assert_eq!(
            entries[0].get("base_instructions").and_then(JsonValue::as_str),
            Some("full prompt")
        );
        assert_eq!(
            entries[0].get("visibility").and_then(JsonValue::as_str),
            Some("list"),
            "visibility 应被覆盖为 list"
        );
        assert_eq!(
            entries[0].get("display_name").and_then(JsonValue::as_str),
            Some("glm-4-plus")
        );
    }
}
