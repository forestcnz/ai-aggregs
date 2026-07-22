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

pub mod form;

pub use form::{CcForm, CcLoadResult, EnvEntry};

use std::path::PathBuf;

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
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
                .unwrap_or_default();
            PathBuf::from(home).join(".claude")
        });
    dir.join("settings.json")
}

// ===================== 表单 ↔ Value 转换 =====================

/// 从 settings.json 的 env 对象中提取扁平 env 数组。
fn extract_env(env: &Map<String, Value>) -> (CcForm, String) {
    let mut entries = Vec::new();
    for (k, v) in env.iter() {
        if let Some(s) = v.as_str() {
            entries.push(EnvEntry {
                key: k.clone(),
                value: s.to_string(),
                secret: None,
            });
        }
    }
    let form = CcForm {
        env: entries,
        file_exists: false,
        raw_json: String::new(),
    };
    (form, serde_json::to_string(env).unwrap_or_default())
}

/// 把表单的 env 段构建为 Value::Object。
fn build_env_obj(form: &CcForm) -> Value {
    let mut env = Map::new();
    for entry in &form.env {
        let k = entry.key.trim();
        let v = entry.value.trim();
        if !k.is_empty() && !v.is_empty() {
            env.insert(k.to_string(), json!(v));
        }
    }
    Value::Object(env)
}

// ===================== 文件读写 =====================

/// 读取并解析 Claude Code 配置文件，提取表单字段。
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
    let root: Value = serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Map::new()));
    let env = root
        .get("env")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let (mut form, raw_json) = extract_env(&env);
    form.file_exists = true;
    form.raw_json = raw_json;
    Ok(CcLoadResult {
        path: path_str,
        exists: true,
        form,
    })
}

/// 把表单写回配置文件：整体替换 `env` 段，其余字段原样保留。
/// - 先读取磁盘当前内容（保留外部可能的修改）
/// - 备份原文件为 `.bak`
/// - 替换 env 段后写回
///
/// 文件不存在时直接以 `{"env": {...}}` 新建。
pub fn save(form: &CcForm) -> std::io::Result<()> {
    let path = config_path();
    // 读取现有内容并解析为 Value（保留未管理字段）
    let mut root: Value = if path.exists() {
        let raw = std::fs::read_to_string(&path)?;
        serde_json::from_str(&raw).unwrap_or_else(|_| Value::Object(Map::new()))
    } else {
        Value::Object(Map::new())
    };
    if !root.is_object() {
        root = Value::Object(Map::new());
    }
    // 整体替换 env 段
    let env_obj = build_env_obj(form);
    // env 段非空才写入（空 env 段则删除，避免无效 { "env": {} }）
    if env_obj.as_object().map(|o| o.is_empty()).unwrap_or(true) {
        root.as_object_mut().map(|o| o.remove("env"));
    } else {
        root.as_object_mut()
            .map(|o| o.insert("env".into(), env_obj));
    }

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
            tracing::warn!(err = %e, "备份 Claude Code 配置失败，继续写入");
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

/// 通过系统 shell 执行 `claude --version`，由 shell 负责按 PATH / PATHEXT 解析
/// （Windows 上 claude 是 npm 生成的 .cmd shim，需经 cmd.exe；Unix 经 sh）。
pub fn version() -> Option<String> {
    let (shell, flag) = if cfg!(target_os = "windows") {
        ("cmd.exe", "/c")
    } else {
        ("sh", "-c")
    };
    let mut cmd = std::process::Command::new(shell);
    cmd.arg(flag)
        .arg("claude --version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let output = cmd.output().ok()?;
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
    fn extract_full_env() {
        let raw = r#"{
            "enabledPlugins": ["@anthropic/claude-code"],
            "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:8000/v1",
                "ANTHROPIC_AUTH_TOKEN": "sk-con...0001",
                "ANTHROPIC_MODEL": "claude-sonnet-4-20250514",
                "ANTHROPIC_SMALL_FAST_MODEL": "claude-haiku-4-20250514",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4-20250514",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4-20250514",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "",
                "CLAUDE_CODE_SUBAGENT_MODEL": "claude-sonnet-4-20250514",
                "CUSTOM_VAR": "custom_value"
            }
        }"#;
        let root: Value = serde_json::from_str(raw).unwrap();
        let env = root.get("env").and_then(Value::as_object).cloned().unwrap();
        let (form, _) = extract_env(&env);
        assert_eq!(form.env.len(), 9);
        assert!(form.env.iter().any(|e| e.key == "ANTHROPIC_BASE_URL"));
        assert!(form.env.iter().any(|e| e.key == "CUSTOM_VAR"));
    }

    #[test]
    fn roundtrip_preserves_extra_and_other_top_keys() {
        let raw = r#"{
            "enabledPlugins": ["@anthropic/claude-code"],
            "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:8000/v1",
                "CUSTOM_VAR": "keep_me"
            }
        }"#;
        let root: Value = serde_json::from_str(raw).unwrap();
        let env = root.get("env").and_then(Value::as_object).cloned().unwrap();
        let (form, _) = extract_env(&env);
        assert_eq!(form.env.len(), 2);

        // 构建 env 对象，验证 CUSTOM_VAR 保留
        let env_obj = build_env_obj(&form);
        let obj = env_obj.as_object().unwrap();
        assert_eq!(obj["CUSTOM_VAR"], "keep_me");
        assert_eq!(obj["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8000/v1");
    }

    #[test]
    fn empty_env_removes_key() {
        let form = CcForm::default();
        let env_obj = build_env_obj(&form);
        assert!(env_obj.as_object().map(|o| o.is_empty()).unwrap_or(true));
    }

    #[test]
    fn save_clears_empty_managed_keys() {
        // 空表单保存时不在 env 对象中产生空字符串 key
        let mut root: Value = serde_json::from_str(
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x"}}"#,
        )
        .unwrap();
        let form = CcForm::default();
        let env_obj = build_env_obj(&form);
        if env_obj.as_object().map(|o| o.is_empty()).unwrap_or(true) {
            root.as_object_mut().map(|o| o.remove("env"));
        } else {
            root.as_object_mut()
                .map(|o| o.insert("env".into(), env_obj));
        }
        let obj = root.as_object().unwrap();
        // env 段被整体删除（因为空）
        assert!(!obj.contains_key("env"), "空 env 段应从 root 删除");
    }
}
