//! OpenCode 配置（opencode.jsonc）读写与网关联动。
//!
//! 仅管理 `~/.config/opencode/opencode.jsonc` 的部分字段：
//!   `model` / `small_model` / `default_agent` / `provider` / `disabled_providers`。
//! 其余字段（`mcp` / `permission` / `agent` / `instructions` / `server` …）
//! 原样保留——保存时 **按 key 合并写入**，不做整体覆盖。
//!
//! 注意：JSONC 注释在解析时被剥离，保存时以纯 JSON 写回，注释会丢失。
//! 保存前自动备份上一份，文件名带时间戳：`opencode.jsonc.YYYYMMDD_HHMMSS.bak`。
//!
//! 路径定位：通过 `HOME` → `USERPROFILE` 环境变量（不依赖 dirs crate，
//! 因 Tauri 的 config_dir 在 Windows 指 %APPDATA%，与 opencode 实际位置不符）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// ===================== 路径 =====================

/// 返回 opencode 全局配置文件路径。
/// 优先 `.jsonc`；不存在则回退到 `.json`（返回期望路径，不强建目录）。
pub fn config_path() -> PathBuf {
    let home = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let base = PathBuf::from(home)
        .join(".config")
        .join("opencode");
    let jsonc = base.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        base.join("opencode.json")
    }
}

// ===================== JSONC 注释剥离 =====================

/// 剥离 JSONC 注释（`//` 行注释与 `/* */` 块注释），尊重字符串字面量。
/// 按 Unicode code point 处理（不会切坏中文 / emoji）。
/// 返回 `(无注释 JSON 文本, 是否曾出现注释)`。
pub fn strip_comments(input: &str) -> (String, bool) {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut had_comment = false;
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        // 字符串字面量：原样拷贝到闭合的未转义 "
        if c == '"' {
            out.push('"');
            i += 1;
            while i < n {
                let ch = chars[i];
                out.push(ch);
                if ch == '\\' && i + 1 < n {
                    // 转义序列，连同下一字符一起拷贝
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                i += 1;
                if ch == '"' {
                    break;
                }
            }
            continue;
        }
        // 行注释 //
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            had_comment = true;
            i += 2;
            while i < n && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // 块注释 /* */
        if c == '/' && i + 1 < n && chars[i + 1] == '*' {
            had_comment = true;
            i += 2;
            while i + 1 < n && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        out.push(c);
        i += 1;
    }
    (out, had_comment)
}

// ===================== 表单数据结构 =====================

/// 表单管理的 opencode 配置子集。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct OcForm {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<String>,
    /// provider 列表（按顺序，保存时转为以 id 为 key 的对象）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<OcProvider>,
    /// 被屏蔽的 provider id 列表（opencode 顶层 `disabled_providers` 字段，
    /// 命中的 provider 即使配置存在也不会被加载）。支持自定义 provider id，
    /// 也支持内置 provider（如 openai/gemini，通过环境变量自动加载的）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_providers: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OcProvider {
    /// provider id（对应 opencode model 字段的前缀，如 "Local-oai"）
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// npm 包名，决定协议 SDK（下拉项）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm: Option<String>,
    #[serde(default)]
    pub options: OcOptions,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<OcModel>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct OcOptions {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "baseURL")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "apiKey")]
    pub api_key: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OcModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub attachment: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub modalities: OcModalities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<OcLimit>,
}

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct OcModalities {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OcLimit {
    pub context: u64,
    pub output: u64,
}

// ===================== 表单 ↔ Value 转换 =====================

/// 规范化 disabled_providers：trim、去空、去重（保留首次出现顺序）。
fn normalize_disabled(raw: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for s in raw {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if seen.insert(t.to_string()) {
            out.push(t.to_string());
        }
    }
    out
}

fn provider_to_value(p: &OcProvider) -> Value {
    let mut po = Map::new();
    if let Some(n) = p.name.as_deref().filter(|s| !s.is_empty()) {
        po.insert("name".into(), json!(n));
    }
    if let Some(npm) = p.npm.as_deref().filter(|s| !s.is_empty()) {
        po.insert("npm".into(), json!(npm));
    }
    let mut opts = Map::new();
    if let Some(b) = p.options.base_url.as_deref().filter(|s| !s.is_empty()) {
        opts.insert("baseURL".into(), json!(b));
    }
    // apiKey 即使留空也写入文件（写 ""），不因空字符串而省略
    opts.insert(
        "apiKey".into(),
        json!(p.options.api_key.as_deref().unwrap_or("")),
    );
    if !opts.is_empty() {
        po.insert("options".into(), Value::Object(opts));
    }
    let mut models = Map::new();
    for m in &p.models {
        if m.id.trim().is_empty() {
            continue;
        }
        models.insert(m.id.clone(), model_to_value(m));
    }
    if !models.is_empty() {
        po.insert("models".into(), Value::Object(models));
    }
    Value::Object(po)
}

fn model_to_value(m: &OcModel) -> Value {
    let mut mo = Map::new();
    if let Some(n) = m.name.as_deref().filter(|s| !s.is_empty()) {
        mo.insert("name".into(), json!(n));
    }
    mo.insert("attachment".into(), json!(m.attachment));
    mo.insert("reasoning".into(), json!(m.reasoning));
    mo.insert("tool_call".into(), json!(m.tool_call));
    let mut modobj = Map::new();
    if !m.modalities.input.is_empty() {
        modobj.insert("input".into(), json!(m.modalities.input));
    }
    if !m.modalities.output.is_empty() {
        modobj.insert("output".into(), json!(m.modalities.output));
    }
    if !modobj.is_empty() {
        mo.insert("modalities".into(), Value::Object(modobj));
    }
    if let Some(l) = &m.limit {
        let mut lim = Map::new();
        lim.insert("context".into(), json!(l.context));
        lim.insert("output".into(), json!(l.output));
        mo.insert("limit".into(), Value::Object(lim));
    }
    Value::Object(mo)
}

/// 从解析后的完整 Value 中提取表单字段（忽略未知字段）。
pub fn extract_form(root: &Value) -> OcForm {
    let Some(obj) = root.as_object() else {
        return OcForm::default();
    };
    let model = obj.get("model").and_then(Value::as_str).map(String::from);
    let small_model = obj
        .get("small_model")
        .and_then(Value::as_str)
        .map(String::from);
    let default_agent = obj
        .get("default_agent")
        .and_then(Value::as_str)
        .map(String::from);
    let mut providers = Vec::new();
    if let Some(Value::Object(pmap)) = obj.get("provider") {
        for (id, pv) in pmap.iter() {
            providers.push(extract_provider(id, pv));
        }
    }
    let disabled_providers = obj
        .get("disabled_providers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    OcForm {
        model,
        small_model,
        default_agent,
        providers,
        disabled_providers,
    }
}

fn extract_provider(id: &str, pv: &Value) -> OcProvider {
    let obj = pv.as_object();
    let name = obj
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .map(String::from);
    let npm = obj
        .and_then(|o| o.get("npm"))
        .and_then(Value::as_str)
        .map(String::from);
    let options = obj
        .and_then(|o| o.get("options"))
        .map(extract_options)
        .unwrap_or_default();
    let mut models = Vec::new();
    if let Some(Value::Object(mmap)) = obj.and_then(|o| o.get("models")) {
        for (mid, mv) in mmap.iter() {
            models.push(extract_model(mid, mv));
        }
    }
    OcProvider {
        id: id.to_string(),
        name,
        npm,
        options,
        models,
    }
}

fn extract_options(v: &Value) -> OcOptions {
    let obj = v.as_object();
    OcOptions {
        base_url: obj
            .and_then(|o| o.get("baseURL"))
            .and_then(Value::as_str)
            .map(String::from),
        api_key: obj
            .and_then(|o| o.get("apiKey"))
            .and_then(Value::as_str)
            .map(String::from),
    }
}

fn extract_model(id: &str, mv: &Value) -> OcModel {
    let obj = mv.as_object();
    let name = obj
        .and_then(|o| o.get("name"))
        .and_then(Value::as_str)
        .map(String::from);
    let attachment = obj
        .and_then(|o| o.get("attachment"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reasoning = obj
        .and_then(|o| o.get("reasoning"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let tool_call = obj
        .and_then(|o| o.get("tool_call"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let modalities = obj
        .and_then(|o| o.get("modalities"))
        .map(extract_modalities)
        .unwrap_or_default();
    let limit = obj.and_then(|o| o.get("limit")).map(extract_limit);
    OcModel {
        id: id.to_string(),
        name,
        attachment,
        reasoning,
        tool_call,
        modalities,
        limit,
    }
}

fn extract_modalities(v: &Value) -> OcModalities {
    let obj = v.as_object();
    let strs = |key: &str| -> Vec<String> {
        obj.and_then(|o| o.get(key))
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default()
    };
    OcModalities {
        input: strs("input"),
        output: strs("output"),
    }
}

fn extract_limit(v: &Value) -> OcLimit {
    let obj = v.as_object();
    let n = |key: &str| {
        obj.and_then(|o| o.get(key))
            .and_then(|x| x.as_u64().or_else(|| x.as_i64().map(|i| i as u64)))
            .unwrap_or(0)
    };
    OcLimit {
        context: n("context"),
        output: n("output"),
    }
}

// ===================== 按 key 合并保存 =====================

/// 把表单管理的字段合并进磁盘读入的 Value（仅覆盖 model / small_model /
/// default_agent / provider / disabled_providers 五个 key，其余字段不动）。
///
/// **清除语义**：表单字段为空时，显式从 root 中删除对应 key
/// （支持用户在 UI 上把某字段置空并保存后，文件里也清掉）。
pub fn merge_form(root: &mut Value, form: &OcForm) {
    let Some(obj) = root.as_object_mut() else {
        return;
    };
    // 字符串字段：非空覆盖、空则删除
    match form.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(m) => {
            obj.insert("model".into(), json!(m));
        }
        None => {
            obj.remove("model");
        }
    }
    match form
        .small_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) => {
            obj.insert("small_model".into(), json!(m));
        }
        None => {
            obj.remove("small_model");
        }
    }
    match form
        .default_agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(a) => {
            obj.insert("default_agent".into(), json!(a));
        }
        None => {
            obj.remove("default_agent");
        }
    }
    // provider 段：有内容则覆盖、空则删除
    let mut providers = Map::new();
    for p in &form.providers {
        if p.id.trim().is_empty() {
            continue;
        }
        providers.insert(p.id.clone(), provider_to_value(p));
    }
    if providers.is_empty() {
        obj.remove("provider");
    } else {
        obj.insert("provider".into(), Value::Object(providers));
    }
    // disabled_providers：非空覆盖、空则删除（trim 去空去重）
    let disabled = normalize_disabled(&form.disabled_providers);
    if disabled.is_empty() {
        obj.remove("disabled_providers");
    } else {
        obj.insert("disabled_providers".into(), json!(disabled));
    }
}

// ===================== 文件读写 =====================

/// load 返回的复合结构。
#[derive(Serialize)]
pub struct OcLoadResult {
    /// 解析后的配置文件绝对路径
    pub path: String,
    /// 文件是否存在（不存在时 form 为空默认值）
    pub exists: bool,
    /// 原文件是否含 JSONC 注释（前端据此提示「注释将丢失」）
    pub has_comments: bool,
    /// 表单数据
    pub form: OcForm,
}

/// 读取并解析 opencode 配置文件，提取表单字段。
/// 文件不存在时返回空表单（exists=false）。
pub fn load() -> std::io::Result<OcLoadResult> {
    let path = config_path();
    let path_str = path.to_string_lossy().to_string();
    if !path.exists() {
        return Ok(OcLoadResult {
            path: path_str,
            exists: false,
            has_comments: false,
            form: OcForm::default(),
        });
    }
    let raw = std::fs::read_to_string(&path)?;
    let (stripped, has_comments) = strip_comments(&raw);
    let root: Value = serde_json::from_str(&stripped).unwrap_or_else(|_| Value::Object(Map::new()));
    let form = extract_form(&root);
    Ok(OcLoadResult {
        path: path_str,
        exists: true,
        has_comments,
        form,
    })
}

/// 把表单按 key 合并写回配置文件。
/// - 先读取磁盘当前内容（合并外部可能的修改）
/// - 备份原文件为 `.bak`
/// - 合并表单字段后以 2 空格 pretty JSON 写回
///
/// 文件不存在时直接以表单字段新建。
pub fn save(form: &OcForm) -> std::io::Result<()> {
    let path = config_path();
    // 读取现有内容并解析为 Value（保留未管理字段）
    let mut root: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        let (stripped, _) = strip_comments(&raw);
        serde_json::from_str(&stripped).unwrap_or_else(|_| Value::Object(Map::new()))
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        // 顶层非对象（异常情况）：以空对象重置
        root = Value::Object(Map::new());
    }
    merge_form(&mut root, form);

    // 备份原文件（存在时）—— 带时间戳，避免多次保存互相覆盖
    if path.exists() {
        // opencode.jsonc → opencode.jsonc.20260717_153045.bak
        let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
        let bak_name = format!("{}.{}.bak", path.file_name().unwrap_or_default().to_string_lossy(), ts);
        let bak_path = path
            .parent()
            .map(|p| p.join(&bak_name))
            .unwrap_or_else(|| std::path::PathBuf::from(&bak_name));
        if let Err(e) = std::fs::copy(&path, &bak_path) {
            tracing::warn!(err = %e, "备份 opencode 配置失败，继续写入");
        }
    }

    // 确保父目录存在
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let pretty = serde_json::to_string_pretty(&root)
        .map_err(|e| std::io::Error::other(format!("序列化失败: {e}")))?;
    std::fs::write(&path, pretty)?;
    Ok(())
}

// ===================== 执行 opencode 命令 =====================

/// 通过系统 shell 执行 `opencode <args>`，由 shell 负责按 PATH / PATHEXT 解析
/// （Windows 上 opencode 是 npm/bun 生成的 .cmd shim，需经 cmd.exe；Unix 经 sh）。
fn run_opencode(args: &str) -> std::io::Result<std::process::Output> {
    let (shell, flag) = if cfg!(target_os = "windows") {
        ("cmd.exe", "/c")
    } else {
        ("sh", "-c")
    };
    let mut cmd = std::process::Command::new(shell);
    cmd.arg(flag)
        .arg(format!("opencode {args}"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(target_os = "windows")]
    hide_console(&mut cmd);
    cmd.output()
}

#[cfg(target_os = "windows")]
fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// 执行 `opencode models`，从其输出（每行 `provider/model`）中同时提取：
///   - `providers`：去重后的 provider id 列表（屏蔽下拉候选）
///   - `models`：去重后的完整 `provider/model` 列表（主/轻量模型下拉候选）
///
/// 一次命令调用同时产出两份数据，避免屏蔽下拉与模型下拉分别执行两次。
///
/// 失败原因：opencode 不在 PATH、命令退出非 0 等。
pub fn list_models_catalog() -> std::io::Result<ModelsCatalog> {
    let output = run_opencode("models")?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let suffix = if err.trim().is_empty() {
            String::new()
        } else {
            format!(": {err}")
        };
        return Err(std::io::Error::other(format!(
            "opencode models 执行失败（退出码 {:?}）{}",
            output.status.code(),
            suffix
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut providers = std::collections::BTreeSet::new();
    let mut models = std::collections::BTreeSet::new();
    for line in text.lines() {
        let l = line.trim();
        if let Some((provider, _)) = l.split_once('/') {
            let p = provider.trim();
            if !p.is_empty() {
                providers.insert(p.to_string());
                models.insert(l.to_string());
            }
        }
    }
    Ok(ModelsCatalog {
        providers: providers.into_iter().collect(),
        models: models.into_iter().collect(),
    })
}

/// opencode 可用 provider / model 目录（一次 `opencode models` 调用的解析结果）
#[derive(Debug, Clone, serde::Serialize)]
pub struct ModelsCatalog {
    /// 去重 provider id 列表（按字母序）
    pub providers: Vec<String>,
    /// 去重完整 `provider/model` 列表（按字母序）
    pub models: Vec<String>,
}

/// 执行 `opencode -v` 获取版本号；未安装或执行失败时返回 None。
/// 前端据此决定是否显示「OpenCode 配置」侧边栏入口。
pub fn version() -> Option<String> {
    let output = run_opencode("-v").ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
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
    fn strip_comments_handles_line_and_block() {
        let input = r#"{
  // 行注释
  "a": 1, /* 块注释 */
  "b": "http://example.com", // url 里的 // 不算注释
  "c": "/* not a comment */"
}"#;
        let (out, had) = strip_comments(input);
        assert!(had);
        assert!(!out.contains("行注释"));
        assert!(!out.contains("块注释"));
        // 字符串内的 // 和 /* 必须保留
        assert!(out.contains("http://example.com"));
        assert!(out.contains("/* not a comment */"));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "http://example.com");
    }

    #[test]
    fn extract_and_merge_roundtrip() {
        let json = r#"{
  "model": "Local-oai/glm-5.2",
  "small_model": "Local-oai/deepseek-v4-flash",
  "provider": {
    "Local-oai": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Local Chat",
      "options": { "baseURL": "http://127.0.0.1:8000/v1", "apiKey": "sk-opencode" },
      "models": {
        "glm-5.2": { "name": "GLM-5.2", "reasoning": true, "tool_call": true,
          "modalities": { "input": ["text"], "output": ["text"] },
          "limit": { "context": 1000000, "output": 131072 } }
      }
    }
  },
  "mcp": { "keep": { "type": "local", "command": ["x"] } }
}"#;
        let root: Value = serde_json::from_str(json).unwrap();
        let form = extract_form(&root);
        assert_eq!(form.model.as_deref(), Some("Local-oai/glm-5.2"));
        assert_eq!(form.providers.len(), 1);
        assert_eq!(form.providers[0].id, "Local-oai");
        assert_eq!(form.providers[0].models.len(), 1);
        let m = &form.providers[0].models[0];
        assert_eq!(m.id, "glm-5.2");
        assert!(m.reasoning);
        assert_eq!(m.modalities.input, vec!["text".to_string()]);
        assert_eq!(m.limit.as_ref().unwrap().context, 1000000);

        // 合并回一个含 mcp 的 root，验证 mcp 被保留
        let mut root2: Value = serde_json::from_str(json).unwrap();
        merge_form(&mut root2, &form);
        assert_eq!(root2["mcp"]["keep"]["command"][0], "x");
        assert_eq!(root2["model"], "Local-oai/glm-5.2");
    }

    #[test]
    fn merge_preserves_unmanaged_keys() {
        let mut root: Value = serde_json::from_str(
            r#"{"model":"a/b","permission":{"edit":"ask"},"instructions":["x.md"]}"#,
        )
        .unwrap();
        let mut form = extract_form(&root);
        form.model = Some("a/c".into());
        merge_form(&mut root, &form);
        // model 被更新
        assert_eq!(root["model"], "a/c");
        // 未管理字段原样保留
        assert_eq!(root["permission"]["edit"], "ask");
        assert_eq!(root["instructions"][0], "x.md");
    }

    #[test]
    fn empty_string_fields_are_dropped() {
        let form = OcForm {
            model: Some("".into()),
            small_model: Some("".into()),
            default_agent: None,
            providers: vec![],
            disabled_providers: vec![],
        };
        // 合并到空对象：空字段不应写入任何 key
        let mut root: Value = Value::Object(Map::new());
        merge_form(&mut root, &form);
        let obj = root.as_object().unwrap();
        assert!(!obj.contains_key("model"));
        assert!(!obj.contains_key("small_model"));
        assert!(obj.is_empty());
    }

    #[test]
    fn ipc_roundtrip_preserves_apikey_camelcase() {
        // 模拟 IPC 全链路：前端发送的 JSON（驼峰 apiKey）→ 反序列化为 OcForm
        // 确认 rename 生效，apiKey 不丢失
        let json_from_frontend = r#"{
            "providers": [{
                "id": "Local-oai",
                "npm": "@ai-sdk/openai-compatible",
                "name": "Local Chat",
                "options": {
                    "baseURL": "http://127.0.0.1:8000/v1",
                    "apiKey": "sk-opencode"
                },
                "models": []
            }]
        }"#;
        let form: OcForm = serde_json::from_str(json_from_frontend).unwrap();
        assert_eq!(form.providers.len(), 1);
        let p = &form.providers[0];
        assert_eq!(p.options.base_url.as_deref(), Some("http://127.0.0.1:8000/v1"));
        assert_eq!(p.options.api_key.as_deref(), Some("sk-opencode"));
    }

    #[test]
    fn merge_empty_field_removes_key() {
        // 用户在 UI 上把 model 置空保存后，文件里 model key 应被删除
        let mut root: Value = serde_json::from_str(
            r#"{"model":"Local-oai/glm-5.2","small_model":"a/b","default_agent":"build","mcp":{"keep":1}}"#,
        )
        .unwrap();
        let form = OcForm {
            model: None,
            small_model: Some("   ".into()), // 纯空白也视为空
            default_agent: None,
            providers: vec![],
            disabled_providers: vec![],
        };
        merge_form(&mut root, &form);
        assert!(root.get("model").is_none(), "model 应被删除");
        assert!(root.get("small_model").is_none(), "small_model 应被删除");
        assert!(root.get("default_agent").is_none(), "default_agent 应被删除");
        assert!(root.get("provider").is_none(), "空 provider 段应被删除");
        // 未管理字段保留
        assert_eq!(root["mcp"]["keep"], 1);
    }

    #[test]
    fn merge_nonempty_field_overwrites() {
        let mut root: Value =
            serde_json::from_str(r#"{"model":"old/model","mcp":{"keep":1}}"#).unwrap();
        let form = OcForm {
            model: Some("new/model".into()),
            small_model: None,
            default_agent: Some("plan".into()),
            providers: vec![],
            disabled_providers: vec![],
        };
        merge_form(&mut root, &form);
        assert_eq!(root["model"], "new/model");
        assert_eq!(root["default_agent"], "plan");
        assert_eq!(root["mcp"]["keep"], 1);
    }

    #[test]
    fn extract_and_merge_disabled_providers() {
        let json = r#"{
  "provider": { "Local-oai": { "options": { "baseURL": "http://127.0.0.1:8000/v1" } } },
  "disabled_providers": ["openai", "Local-oai", "openai", "  "],
  "mcp": { "keep": 1 }
}"#;
        let root: Value = serde_json::from_str(json).unwrap();
        let form = extract_form(&root);
        // 提取时原样保留（含重复与空白），规范在合并阶段
        assert_eq!(form.disabled_providers.len(), 4);
        assert!(form.disabled_providers.contains(&"openai".to_string()));
        assert!(form.disabled_providers.contains(&"Local-oai".to_string()));

        // 合并阶段：trim、去空、去重
        let mut root2: Value = serde_json::from_str(json).unwrap();
        merge_form(&mut root2, &form);
        let dp = root2["disabled_providers"].as_array().unwrap();
        assert_eq!(dp.len(), 2, "应去重去空");
        assert!(dp.iter().any(|v| v == "openai"));
        assert!(dp.iter().any(|v| v == "Local-oai"));
        // 未管理字段保留
        assert_eq!(root2["mcp"]["keep"], 1);
    }

    #[test]
    fn merge_empty_disabled_providers_removes_key() {
        let mut root: Value = serde_json::from_str(
            r#"{"disabled_providers":["openai"],"mcp":{"keep":1}}"#,
        )
        .unwrap();
        let form = OcForm {
            disabled_providers: vec![],
            ..Default::default()
        };
        merge_form(&mut root, &form);
        assert!(root.get("disabled_providers").is_none(), "空数组应删除 key");
        assert_eq!(root["mcp"]["keep"], 1);
    }
}
