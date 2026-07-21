//! 进程管理公用工具：执行外部命令、隐藏控制台窗口、备份文件。
//!
//! 集中管理 `run_opencode` / `run_claude` / `run_codex` 三处重复的
//! shell 执行逻辑、`hide_console` 窗口隐藏、`backup_file` 时间戳备份。

use std::path::Path;

/// 通过系统 shell 执行 `<bin> <args>`，由 shell 负责按 PATH / PATHEXT 解析
///（Windows 上 npm/bun shim 需经 cmd.exe；Unix 经 sh）。
pub fn run_bin(bin: &str, args: &str) -> std::io::Result<std::process::Output> {
    let (shell, flag) = if cfg!(target_os = "windows") {
        ("cmd.exe", "/c")
    } else {
        ("sh", "-c")
    };
    let mut cmd = std::process::Command::new(shell);
    cmd.arg(flag)
        .arg(format!("{bin} {args}"))
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

/// 备份原文件（存在时）—— 带时间戳，避免多次保存互相覆盖。
/// `file.json` → `file.json.YYYYMMDD_HHMMSS.bak`
pub fn backup_file(path: &Path) {
    if !path.exists() {
        return;
    }
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
    if let Err(e) = std::fs::copy(path, &bak_path) {
        tracing::warn!(err = %e, "备份配置文件失败，继续写入");
    }
}
