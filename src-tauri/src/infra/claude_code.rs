//! Claude Code 配置（~/.claude/settings.json）读写与网关联动。
//!
//! 仅管理 `settings.json` 中的 **`env` 段**（环境变量）——这是把 Claude Code
//! 指向本网关的关键：`ANTHROPIC_BASE_URL` + `ANTHROPIC_AUTH_TOKEN` +
//! 各模型变量（`ANTHROPIC_MODEL` / `ANTHROPIC_SMALL_FAST_MODEL` /
//! `ANTHROPIC_DEFAULT_{HAIKU,SONNET,OPUS}_MODEL` / `CLAUDE_CODE_SUBAGENT_MODEL`）。
//!
//! 其余顶层字段（`enabledPlugins` / `statusLine` / `permissions` /
//! `hasCompletedOnboarding` …）原样保留——保存时 **整体替换 `env` 段**，
//! 其它 key 一律不动。env 段中表单未展示的「自定义变量」也会被原样带回
//! （表单加载全部 env 条目，含未知 key）。
//!
//! 保存前自动备份上一份，文件名带时间戳：`settings.json.YYYYMMDD_HHMMSS.bak`。
//!
//! 路径定位：优先 `CLAUDE_CONFIG_DIR` 环境变量（Claude Code 官方支持），
//! 否则回退到 `~/.claude/settings.json`（`HOME` → `USERPROFILE`）。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

// ===================== 路径 =====================

/// 返回 Claude Code 用户级配置文件路径。
/// 优先 `CLAUDE_CONFIG_DIR`；否则 `$HOME/.claude/settings.json`
/// （`HOME` → `USERPROFILE`）。返回期望路径，不强建目录。
pub fn config_path() -> PathBuf {
    let dir = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let home = std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
                .unwrap_or_default();
            Some(PathBuf::from(home).join(".claude"))
        })
        .unwrap_or_else(|| PathBuf::from(".claude"));
    dir.join("settings.json")
}

// ===================== 表单数据结构 =====================

/// 表单管理的 Claude Code 配置子集：仅 `env` 段。
///
/// env 在文件中是「字符串 → 字符串」的对象；表单用有序条目列表承载，
/// 以便前端逐行增删并保留书写顺序。保存时整体写回为对象。
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct CcForm {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<CcEnvEntry>,
}

/// 一条环境变量。`secret` 仅作为前端展示提示（不参与序列化），
/// 由后端在提取时按 key 名（含 TOKEN/KEY/SECRET）推断。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CcEnvEntry {
    pub key: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 判断 key 是否为敏感凭证（前端据此掩码展示）。
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    k.contains("TOKEN") || k.contains("SECRET") || k.contains("PASSWORD")
}

// ===================== 表单 ↔ Value 转换 =====================

/// 把任意 JSON 标量规范成字符串（env 值理论上是字符串，
/// 但兜底处理 bool / number，避免丢失）。
fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None, // null / array / object：env 段不会出现，跳过
    }
}

/// 从解析后的完整 Value 中提取 `env` 段为有序条目列表（忽略未知字段）。
pub fn extract_form(root: &Value) -> CcForm {
    let Some(env_obj) = root.get("env").and_then(Value::as_object) else {
        return CcForm::default();
    };
    let mut env = Vec::with_capacity(env_obj.len());
    for (k, v) in env_obj.iter() {
        if let Some(val) = scalar_to_string(v) {
            env.push(CcEnvEntry {
                key: k.clone(),
                value: val,
                secret: is_secret_key(k),
            });
        }
    }
    CcForm { env }
}

/// 把表单的 env 段整体写回 root（仅替换 `env` key，其它顶层字段不动）。
///
/// **清除语义**：表单 env 为空时，显式从 root 中删除 `env` key
/// （支持用户在 UI 上删光全部变量并保存后，文件里也清掉）。
/// 空 key（trim 后为空）的条目被跳过。
pub fn merge_form(root: &mut Value, form: &CcForm) {
    let Some(obj) = root.as_object_mut() else {
        return;
    };
    let mut env_map = Map::new();
    for e in &form.env {
        let key = e.key.trim();
        if key.is_empty() {
            continue;
        }
        env_map.insert(key.to_string(), json!(e.value));
    }
    if env_map.is_empty() {
        obj.remove("env");
    } else {
        obj.insert("env".into(), Value::Object(env_map));
    }
}

// ===================== 文件读写 =====================

/// load 返回的复合结构。
#[derive(Serialize)]
pub struct CcLoadResult {
    /// 解析后的配置文件绝对路径
    pub path: String,
    /// 文件是否存在（不存在时 form 为空默认值）
    pub exists: bool,
    /// 表单数据
    pub form: CcForm,
}

/// 读取并解析 Claude Code 配置文件，提取 env 段。
/// 文件不存在时返回空表单（exists=false）。
pub fn load() -> std::io::Result<CcLoadResult> {
    let path = config_path();
    let path_str = path.to_string_lossy().to_string();
    if !path.exists() {
        return Ok(CcLoadResult {
            path: path_str,
            exists: false,
            form: CcForm::default(),
        });
    }
    let raw = std::fs::read_to_string(&path)?;
    // settings.json 是严格 JSON（无注释），解析失败时退回空对象，避免阻塞 UI
    let root: Value =
        serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Map::new()));
    let form = extract_form(&root);
    Ok(CcLoadResult {
        path: path_str,
        exists: true,
        form,
    })
}

/// 把表单合并写回配置文件（整体替换 env 段，保留其它顶层字段）。
/// - 先读取磁盘当前内容（保留外部可能的修改）
/// - 备份原文件为 `.bak`
/// - 合并 env 后以 2 空格 pretty JSON 写回
///
/// 文件不存在时直接以表单字段新建。
pub fn save(form: &CcForm) -> std::io::Result<()> {
    let path = config_path();
    let mut root: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Map::new()))
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    merge_form(&mut root, form);

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
            tracing::warn!(err = %e, "备份 claude code 配置失败，继续写入");
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

// ===================== 执行 claude 命令 =====================

/// 通过系统 shell 执行 `claude <args>`，由 shell 负责按 PATH / PATHEXT 解析
/// （Windows 上 claude 是 npm 生成的 .cmd shim，需经 cmd.exe；Unix 经 sh）。
fn run_claude(args: &str) -> std::io::Result<std::process::Output> {
    let (shell, flag) = if cfg!(target_os = "windows") {
        ("cmd.exe", "/c")
    } else {
        ("sh", "-c")
    };
    std::process::Command::new(shell)
        .arg(flag)
        .arg(format!("claude {args}"))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
}

/// 执行 `claude --version` 获取版本号；未安装或执行失败时返回 None。
/// 输出形如 `2.1.170 (Claude Code)`，取首个空白分隔的 token（即 `2.1.170`）。
/// 前端据此决定是否显示「Claude Code 配置」侧边栏入口。
pub fn version() -> Option<String> {
    let output = run_claude("--version").ok()?;
    if !output.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("")
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
    fn extract_env_into_ordered_entries() {
        let root: Value = serde_json::from_str(
            r#"{
          "env": {
            "ANTHROPIC_BASE_URL": "http://127.0.0.1:8000",
            "ANTHROPIC_AUTH_TOKEN": "sk-cc",
            "ANTHROPIC_MODEL": "glm-5.2",
            "API_TIMEOUT_MS": "3000000"
          },
          "enabledPlugins": { "x": true },
          "hasCompletedOnboarding": true
        }"#,
        )
        .unwrap();
        let form = extract_form(&root);
        assert_eq!(form.env.len(), 4);
        // 敏感 key 标记
        let token = form.env.iter().find(|e| e.key == "ANTHROPIC_AUTH_TOKEN").unwrap();
        assert!(token.secret);
        assert_eq!(token.value, "sk-cc");
        let model = form.env.iter().find(|e| e.key == "ANTHROPIC_MODEL").unwrap();
        assert!(!model.secret);
    }

    #[test]
    fn extract_stringifies_non_string_scalars() {
        let root: Value = serde_json::from_str(
            r#"{"env":{"FLAG":true,"COUNT":42,"NAME":"x"}}"#,
        )
        .unwrap();
        let form = extract_form(&root);
        let by_key = |k: &str| form.env.iter().find(|e| e.key == k).unwrap().value.clone();
        assert_eq!(by_key("FLAG"), "true");
        assert_eq!(by_key("COUNT"), "42");
        assert_eq!(by_key("NAME"), "x");
    }

    #[test]
    fn merge_replaces_env_and_preserves_others() {
        let mut root: Value = serde_json::from_str(
            r#"{
          "env": { "OLD": "x", "ANTHROPIC_MODEL": "old-model" },
          "enabledPlugins": { "x": true },
          "statusLine": { "type": "command" }
        }"#,
        )
        .unwrap();
        let form = CcForm {
            env: vec![
                CcEnvEntry {
                    key: "ANTHROPIC_BASE_URL".into(),
                    value: "http://127.0.0.1:8000".into(),
                    secret: false,
                },
                CcEnvEntry {
                    key: "ANTHROPIC_MODEL".into(),
                    value: "glm-5.2".into(),
                    secret: false,
                },
            ],
        };
        merge_form(&mut root, &form);
        // env 被整体替换：OLD 消失，新值写入
        assert_eq!(root["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8000");
        assert_eq!(root["env"]["ANTHROPIC_MODEL"], "glm-5.2");
        assert!(root["env"].get("OLD").is_none(), "OLD 应被替换掉");
        // 其它顶层字段保留
        assert_eq!(root["enabledPlugins"]["x"], true);
        assert_eq!(root["statusLine"]["type"], "command");
    }

    #[test]
    fn merge_empty_env_removes_key() {
        let mut root: Value =
            serde_json::from_str(r#"{"env":{"A":"1"},"enabledPlugins":{"x":true}}"#).unwrap();
        let form = CcForm::default();
        merge_form(&mut root, &form);
        assert!(root.get("env").is_none(), "空 env 应删除 key");
        assert_eq!(root["enabledPlugins"]["x"], true);
    }

    #[test]
    fn merge_skips_empty_keys() {
        let mut root: Value = Value::Object(Map::new());
        let form = CcForm {
            env: vec![
                CcEnvEntry {
                    key: "   ".into(),
                    value: "v".into(),
                    secret: false,
                },
                CcEnvEntry {
                    key: "GOOD".into(),
                    value: "v".into(),
                    secret: false,
                },
            ],
        };
        merge_form(&mut root, &form);
        assert_eq!(root["env"]["GOOD"], "v");
        assert!(root["env"].as_object().unwrap().len() == 1);
    }

    #[test]
    fn roundtrip_preserves_custom_env_keys() {
        // 用户在 env 里有一个自定义变量，表单应原样带回
        let json = r#"{
          "env": { "ANTHROPIC_MODEL": "glm-5.2", "MY_CUSTOM": "abc" },
          "enabledPlugins": { "x": true }
        }"#;
        let root: Value = serde_json::from_str(json).unwrap();
        let form = extract_form(&root);
        assert_eq!(form.env.len(), 2);
        // 未改动地合并回去：自定义变量仍在
        let mut root2: Value = serde_json::from_str(json).unwrap();
        merge_form(&mut root2, &form);
        assert_eq!(root2["env"]["MY_CUSTOM"], "abc");
        assert_eq!(root2["env"]["ANTHROPIC_MODEL"], "glm-5.2");
        assert_eq!(root2["enabledPlugins"]["x"], true);
    }

    #[test]
    fn secret_key_detection() {
        assert!(is_secret_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(is_secret_key("MY_SECRET"));
        assert!(is_secret_key("api_password"));
        assert!(!is_secret_key("ANTHROPIC_MODEL"));
        assert!(!is_secret_key("ANTHROPIC_BASE_URL"));
    }
}
