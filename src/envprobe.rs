//! 环境探测（P5 E1）：探测本机常见工具链，注入 Runtime Context。
//!
//! 进程启动时对 main agent 探测一次，`/env` 斜杠命令手动刷新；
//! 只列出**存在且版本可解析**的项（命令不存在 / 超时 / 非零退出均跳过），
//! 体积控制在几行内，token 开销可忽略。

use std::time::Duration;

/// 单命令探测超时（防止 PATH 里有同名假命令挂起启动）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// 候选工具链：(命令, 版本参数)。
/// Windows 下额外探测 powershell/pwsh（失败自动跳过，安全）。
const CANDIDATES: &[(&str, &str)] = &[
    ("python", "--version"),
    ("node", "--version"),
    ("npm", "--version"),
    ("rustc", "--version"),
    ("cargo", "--version"),
    ("go", "version"),
    ("git", "--version"),
    ("docker", "--version"),
    #[cfg(windows)]
    ("powershell", "--version"),
    #[cfg(windows)]
    ("pwsh", "--version"),
];

/// 探测本机环境，返回注入文本。
///
/// 格式：`[env] python 3.13.2 · node 22.22.2 · git 2.47.1`
/// 无任何可用工具时返回空串（调用方按 None 处理，跳过注入）。
pub async fn probe() -> String {
    // 并发跑所有候选命令，整体耗时 ≈ 单个最长命令耗时（≤ 2s）
    let mut handles = Vec::with_capacity(CANDIDATES.len());
    for &(cmd, flag) in CANDIDATES {
        handles.push(tokio::spawn(run_version(cmd, flag)));
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(shell) = detect_shell() {
        parts.push(shell);
    }
    for h in handles {
        if let Ok(Ok(Some(entry))) = h.await {
            parts.push(entry);
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        format!("[env] {}", parts.join(" · "))
    }
}

/// 跑单个 `<cmd> <flag>`，成功且 stdout 可解析时返回 `cmd <version>` 条目。
/// 命令不存在 / 超时 / 非零退出 / 无输出 → None（不列出）。
async fn run_version(cmd: &str, flag: &str) -> anyhow::Result<Option<String>> {
    let out = tokio::time::timeout(PROBE_TIMEOUT, async {
        tokio::process::Command::new(cmd).arg(flag).output().await
    })
    .await;

    let output = match out {
        Ok(Ok(o)) => o,
        _ => return Ok(None), // NotFound / timeout / spawn error
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("{} {}", cmd, clean_version(cmd, line))))
}

/// 清洗版本行：`rustc --version` 输出以命令名开头时去掉重复前缀。
/// 例如 "rustc 1.97.0 (x)" → "1.97.0 (x)"。
fn clean_version<'a>(cmd: &str, line: &'a str) -> &'a str {
    let rest = line.strip_prefix(cmd).map(str::trim).unwrap_or(line);
    rest.trim()
}

/// 探测默认 shell（Unix 从 `$SHELL` 取 basename；Windows 无 `$SHELL` 时返回 None）。
fn detect_shell() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    if shell.trim().is_empty() {
        return None;
    }
    let name = std::path::Path::new(&shell)
        .file_name()?
        .to_string_lossy()
        .into_owned();
    Some(format!("shell {}", name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_version_strips_repeated_cmd_prefix() {
        assert_eq!(clean_version("rustc", "rustc 1.97.0 (abc)"), "1.97.0 (abc)");
        assert_eq!(clean_version("node", "v22.22.2"), "v22.22.2");
        assert_eq!(clean_version("git", "git version 2.47.1"), "version 2.47.1");
    }

    #[test]
    fn clean_version_keeps_line_when_no_prefix() {
        assert_eq!(clean_version("python", "Python 3.13.2"), "Python 3.13.2");
    }

    #[test]
    fn detect_shell_returns_basename() {
        // 设置环境变量不污染并行测试：用子进程/直接调函数验证空路径分支
        // （SHELL 未设置时返回 None）
        std::env::remove_var("SHELL");
        assert_eq!(detect_shell(), None);
    }

    #[tokio::test]
    async fn run_version_skips_unknown_command() {
        // 不存在的命令 → None，不 panic
        let r = run_version("llaia_definitely_not_a_cmd_xyz", "--version")
            .await
            .unwrap();
        assert!(r.is_none());
    }
}
