//! 交互式审批门控（P4-d）。
//!
//! 权限档位（profile）判定哪些操作需要审批，审批时注册 `PendingApproval`，
//! 用户通过 `/ok <id>` / `/deny <id>` 解析。审批门控独立于 agent 锁，
//! 以便非 CLI 频道也能在不死锁的情况下解析。

use crate::tools::Tool;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// pending 类型：审批（待执行操作需批准）或提问（ask_user 待回答）。
/// 二者共用注册表与续跑机制，但语义/UI/路由不同。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingKind {
    Approval,
    Question,
}

/// 一次待确认/待回答的请求
#[derive(Clone)]
pub struct PendingApproval {
    pub id: String,
    pub kind: PendingKind,
    pub tool_name: String,
    pub args: Value,
    /// 原始 tool_call 的 id（用于把结果写回对应 tool 消息）
    pub tool_call_id: String,
    pub channel: String,
    pub agent_alias: String,
    /// 操作是否落在 workspace 内（用于审批提示文案）
    pub within_workspace: bool,
    /// ask_user 的问题文本（kind==Question 时有效）
    pub question: String,
    /// ask_user 的可选结构化单选选项（kind==Question 时有效）
    pub choices: Option<Vec<String>>,
    /// 注册时的 unix 时间戳（秒），用于超时判定
    pub created_at: u64,
    /// 超时秒数（kind==Question 时有效；0 = 不超时）
    pub timeout_secs: u64,
}

/// 审批门控：保存当前待确认请求。独立锁，避免与 agent 锁相互阻塞。
pub struct ApprovalGate {
    inner: Mutex<HashMap<String, PendingApproval>>,
    counter: AtomicU64,
}

impl ApprovalGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            counter: AtomicU64::new(0),
        })
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 注册一条待确认（操作）请求，返回 id
    pub async fn register(
        &self,
        tool_name: &str,
        args: &Value,
        tool_call_id: &str,
        channel: &str,
        agent_alias: &str,
        within_workspace: bool,
    ) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("ap{}", n);
        let pending = PendingApproval {
            id: id.clone(),
            kind: PendingKind::Approval,
            tool_name: tool_name.to_string(),
            args: args.clone(),
            tool_call_id: tool_call_id.to_string(),
            channel: channel.to_string(),
            agent_alias: agent_alias.to_string(),
            within_workspace,
            question: String::new(),
            choices: None,
            created_at: Self::now_secs(),
            timeout_secs: 0,
        };
        self.inner.lock().await.insert(id.clone(), pending);
        id
    }

    /// 注册一条待回答（ask_user）请求，返回 id。
    /// `timeout_secs` 为 0 表示永不超时。
    pub async fn register_question(
        &self,
        question: &str,
        choices: Option<Vec<String>>,
        channel: &str,
        agent_alias: &str,
        timeout_secs: u64,
    ) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("q{}", n);
        let pending = PendingApproval {
            id: id.clone(),
            kind: PendingKind::Question,
            tool_name: "ask_user".to_string(),
            args: Value::Null,
            tool_call_id: String::new(),
            channel: channel.to_string(),
            agent_alias: agent_alias.to_string(),
            within_workspace: false,
            question: question.to_string(),
            choices,
            created_at: Self::now_secs(),
            timeout_secs,
        };
        self.inner.lock().await.insert(id.clone(), pending);
        id
    }

    /// 取出一条待确认请求（消费式，避免重复处理）
    pub async fn take(&self, id: &str) -> Option<PendingApproval> {
        self.inner.lock().await.remove(id)
    }

    /// 取出一条待回答问题（仅 Question 类，消费式）
    pub async fn take_question(&self, id: &str) -> Option<PendingApproval> {
        let mut g = self.inner.lock().await;
        match g.get(id) {
            Some(p) if p.kind == PendingKind::Question => g.remove(id),
            _ => None,
        }
    }

    /// 列出当前所有待确认/待回答（用于审计/调试）
    pub async fn list(&self) -> Vec<PendingApproval> {
        self.inner.lock().await.values().cloned().collect()
    }

    /// 列出所有待回答问题
    pub async fn questions(&self) -> Vec<PendingApproval> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|p| p.kind == PendingKind::Question)
            .cloned()
            .collect()
    }

    /// 若当前恰有一个待回答问题，返回它（用于"单 pending 时用户下一条消息即答案"）。
    /// 多个或零个 pending 时返回 None（需显式 /answer <id>）。
    pub async fn single_question(&self) -> Option<PendingApproval> {
        let qs = self.questions().await;
        if qs.len() == 1 {
            Some(qs.into_iter().next().unwrap())
        } else {
            None
        }
    }

    /// 判断一条待回答问题是否已超时（timeout_secs>0 且超过）
    pub fn is_question_expired(p: &PendingApproval, now: u64) -> bool {
        p.kind == PendingKind::Question
            && p.timeout_secs > 0
            && now.saturating_sub(p.created_at) > p.timeout_secs
    }
}

/// 审批上下文：把 `execute_tool_calls` 所需的审批相关参数打包，
/// 避免参数过多触发 clippy `too_many_arguments`。
pub struct ApprovalContext {
    pub profile: String,
    pub workspace: PathBuf,
    /// 会话级受信目录集合（#B）：/move 批准过的目标目录，canonical 形态。
    /// 判定「是否在 workspace 内」时与 `workspace` 同等对待，令目录内操作免审批。
    pub trusted: Vec<PathBuf>,
    pub gate: Arc<ApprovalGate>,
    pub agent_alias: String,
    pub audit: Option<Arc<crate::audit::AuditLog>>,
    /// ask_user 超时秒数（ADR-0022），构造自 `[runtime].ask_user_timeout_secs`
    pub ask_user_timeout_secs: u64,
}

/// 是否交互式频道（能等待用户 /ok /deny /answer）
pub fn is_interactive_channel(channel: &str) -> bool {
    matches!(
        channel,
        "cli" | "qq" | "telegram" | "dingtalk" | "wechat" | "feishu" | "web"
    )
}

/// 判定工具操作是否落在 workspace 或任一受信目录内（用于 default 档位的审批范围）。
///
/// `trusted` 为会话级受信目录集合（#B）：/move 批准过的目标目录，与 `workspace`
/// 同等对待——落在其中任一目录内的操作自动放行，逃出全部范围的仍需审批。
pub fn tool_within_workspace(
    tool_name: &str,
    args: &Value,
    workspace: &Path,
    trusted: &[PathBuf],
) -> bool {
    match tool_name {
        "file_write" | "file_edit" | "file_read" => {
            let p = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            path_within_any(workspace, trusted, |ws| {
                crate::path_guard::validate_path(ws, p, None).is_ok()
            })
        }
        "terminal" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("");
            // 不把 skills_dir 作为 extra_readable 传进来：terminal 引用 skill 目录
            // 脚本仍判为 workspace 外 → 走审批（执行 skill 代码需人肉确认，符合 ADR-0028）。
            path_within_any(workspace, trusted, |ws| {
                crate::path_guard::validate_command_paths(cmd, ws, None).is_ok()
            })
        }
        // MCP 工具一律视为 workspace 外（ADR-0020：安全默认，需审批）
        name if name.starts_with("mcp_") => false,
        // 其它（memory_write / send_image / send_file / delegate / web_fetch / tavily / 子 agent）
        // 一律视为在 workspace 内
        _ => true,
    }
}

/// 依次以 workspace、各受信目录为基准跑同一个校验闭包，任一通过即视为「在范围内」。
fn path_within_any(
    workspace: &Path,
    trusted: &[PathBuf],
    check: impl Fn(&Path) -> bool,
) -> bool {
    if check(workspace) {
        return true;
    }
    trusted.iter().any(|d| check(d))
}

/// 审批决策
pub enum ApprovalAction {
    /// 直接执行，无需审批
    Approved,
    /// 需要审批（交互式频道注册 pending，非交互式频道自动拒绝）
    NeedsApproval { within_workspace: bool },
    /// 自动拒绝（非交互式频道无法等待用户）
    Denied { reason: String },
}

/// 根据权限档位 + 工具副作用 + workspace/受信目录范围，决定一次工具调用是否需要审批
pub fn approval_decision(
    tool: &dyn Tool,
    args: &Value,
    workspace: &Path,
    trusted: &[PathBuf],
    profile: &str,
    channel: &str,
) -> ApprovalAction {
    // 子 agent 委派：不受审批拦截（与 P2-a 一致，channel 固定为 "delegate"）
    if channel == "delegate" {
        return ApprovalAction::Approved;
    }
    if profile == "yolo" {
        return ApprovalAction::Approved;
    }
    if !tool.requires_confirm() {
        return ApprovalAction::Approved;
    }
    let within = tool_within_workspace(tool.name(), args, workspace, trusted);
    let required = match profile {
        "read-only" => true,
        // default 或未识别档位：仅 workspace 外需审批
        _ => !within,
    };
    if !required {
        return ApprovalAction::Approved;
    }
    if is_interactive_channel(channel) {
        ApprovalAction::NeedsApproval {
            within_workspace: within,
        }
    } else {
        ApprovalAction::Denied {
            reason: format!(
                "operation `{}` requires approval, but channel `{}` is non-interactive and cannot wait for confirmation; skipped",
                tool.name(),
                channel
            ),
        }
    }
}

/// 简短摘要（用于提示文案），避免把完整 JSON 推给用户
fn summarize_args(tool_name: &str, args: &Value) -> String {
    match tool_name {
        "file_write" | "file_edit" => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        "terminal" => args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string(),
        "memory_write" => args
            .get("entry")
            .and_then(|v| v.as_str())
            .map(|s| {
                let t = s.trim();
                if t.len() > 40 {
                    format!("{}…", &t[..40])
                } else {
                    t.to_string()
                }
            })
            .unwrap_or_else(|| "?".to_string()),
        _ => {
            // 取第一个非空的标量值作为摘要
            if let Some(obj) = args.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            return format!("{}={}", k, s);
                        }
                    }
                }
            }
            "…".to_string()
        }
    }
}

/// 生成给用户看的审批提示文案
pub fn format_approval_prompt(
    tool_name: &str,
    args: &Value,
    workspace: &Path,
    id: &str,
    within_workspace: bool,
) -> String {
    let summary = summarize_args(tool_name, args);
    let scope = if within_workspace {
        "within workspace"
    } else {
        "outside workspace"
    };
    // 长命令/长路径截断到一行，避免 QQ/Telegram 等频道把审批提示拆成多条消息。
    const SUMMARY_CAP: usize = 220;
    let summary = if summary.chars().count() > SUMMARY_CAP {
        let cut: String = summary.chars().take(SUMMARY_CAP).collect();
        format!("{}… (truncated)", cut)
    } else {
        summary
    };
    let target = match tool_name {
        "terminal" => format!("command: {}", summary),
        _ => format!("target: {}", summary),
    };
    format!(
        "\n🔐 Requesting approval to execute `{}` ({})\n   {}\n   workspace: {}\n   Reply `/ok {}` to approve or `/deny {}` to reject. When only one approval is pending, a bare `/ok` or `/deny` works.\n",
        tool_name,
        scope,
        target,
        workspace.display(),
        id,
        id
    )
}

/// 格式化 /move 审批提示
pub fn format_move_prompt(new_workspace: &Path, id: &str) -> String {
    format!(
        "\n🔀 Requesting approval to switch the working directory (workspace) to:\n   {}\n   ⚠️ After switching, file/terminal tools will only operate inside this directory; operations within it are approved by default (no per-step confirmation). Only paths touching outside the directory still require approval. The directory is remembered as trusted for this session, so switching back later keeps it auto-approved.\n   Reply `/ok {id}` to confirm the switch or `/deny {id}` to cancel. When only one approval is pending, a bare `/ok` or `/deny` works.\n",
        new_workspace.display(),
    )
}

/// 校验 /move 目标目录：必须真实存在且非危险前缀
pub fn validate_move_target(path: &str) -> anyhow::Result<PathBuf> {
    let p = PathBuf::from(path);
    let abs = if p.is_absolute() {
        p.clone()
    } else {
        std::env::current_dir()?.join(p)
    };
    if !abs.exists() {
        anyhow::bail!("directory does not exist: {}", abs.display());
    }
    if !abs.is_dir() {
        anyhow::bail!("not a directory: {}", abs.display());
    }
    let canon = std::fs::canonicalize(&abs)?;
    if crate::path_guard::hits_blacklist(&canon.to_string_lossy()) {
        anyhow::bail!(
            "target directory hits the dangerous-path blacklist, refusing to switch: {}",
            canon.display()
        );
    }
    // Windows canonicalize 返回带 `\\?\` verbatim 前缀；若直接存进 workspace_root，
    // 后续 validate_path 里 norm_ws（保留前缀）与命令路径 canonicalize 后 strip 的
    // 结果（无前缀）永远 starts_with 不匹配 → moved 目录内的绝对路径操作全部误判为
    // workspace 外、每次都要审批。统一剥掉前缀，保持普通路径形态。
    Ok(crate::path_guard::strip_verbatim_prefix(&canon))
}

// 本模块的测试全部依赖 Windows 路径/行为（verbatim `\\?\`、真实 WAS 目录命令），
// 整模块门控到 windows + test，避免 Linux 上出现未使用的 `super::*`/`json` 导入告警。
#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use serde_json::json;

    // 平台专属：/move 到某目录后，目录内绝对路径命令必须判定为 workspace 内
    // （default 档免审批）；且 validate_move_target 不得携带 Windows verbatim
    // `\\?\` 前缀，否则与命令路径形态不一致，moved 目录内操作每次都要审批。
    #[test]
    fn test_move_trusts_moved_dir_within_scope() {
        let dir = tempfile::tempdir().expect("tempdir");

        let moved = validate_move_target(dir.path().to_str().unwrap()).expect("move");
        assert!(
            !moved.to_string_lossy().contains(r"\\?\"),
            "moved workspace 不应带 verbatim 前缀，实际: {}",
            moved.display()
        );

        // 目录内绝对路径的终端命令应视为 workspace 内 → default 档免审批
        let cmd = format!("cat {}", dir.path().join("notes.txt").display());
        let within = tool_within_workspace("terminal", &json!({ "command": cmd }), &moved, &[]);
        assert!(within, "moved 目录内绝对路径命令应判定为 workspace 内");
    }

    // #B 受信目录：/move 批准过的目录加入受信集合后，即使 workspace 已切走，
    // 落在受信目录内的 file/terminal 操作仍免审批；逃出全部范围的仍需审批。
    #[test]
    fn test_trusted_dirs_keep_moved_dir_approved_after_switch_away() {
        let trusted_dir = tempfile::tempdir().expect("tempdir");
        let other_dir = tempfile::tempdir().expect("tempdir");

        let trusted = validate_move_target(trusted_dir.path().to_str().unwrap()).expect("move");
        let current_ws =
            validate_move_target(other_dir.path().to_str().unwrap()).expect("move");
        let trusted_set = vec![trusted.clone()];

        // 受信目录内的读文件 → 免审批（即便 workspace_root 已切到别处）
        let file_in_trusted = trusted.join("doc.md").to_string_lossy().replace('\\', "/");
        let within = tool_within_workspace(
            "file_read",
            &json!({ "path": file_in_trusted }),
            &current_ws,
            &trusted_set,
        );
        assert!(within, "受信目录内的文件读取应免审批");

        // 受信目录内的终端命令 → 免审批
        let cmd = format!("ls {}", trusted_dir.path().join("sub").display());
        let within = tool_within_workspace("terminal", &json!({ "command": cmd }), &current_ws, &trusted_set);
        assert!(within, "受信目录内的终端命令应免审批");

        // 同一操作在受信集合为空时仍判为外 → 需审批（对照）
        let without_trust = tool_within_workspace(
            "file_read",
            &json!({ "path": file_in_trusted }),
            &current_ws,
            &[],
        );
        assert!(!without_trust, "无受信记录时，受信目录外的读取应需审批");

        // 逃出 workspace 与全部受信目录 → 需审批
        let outside = other_dir.path().join("../elsewhere.txt").to_string_lossy().replace('\\', "/");
        let within = tool_within_workspace(
            "file_read",
            &json!({ "path": outside }),
            &current_ws,
            &trusted_set,
        );
        assert!(!within, "逃出 workspace 与受信集合的操作仍应需审批");
    }

    // 用 db 里真实 /move 后的命令在真实 moved 目录上验证：命令都以 `cd "E:/<moved>" &&`
    // 开头，cd 目标就是 moved 目录本身，应判定 workspace 内（免审批）。
    // 若此断言成立，运行时那些审批必然来自「越出 moved 目录」的其它路径（如 find E:/s、
    // 读 home workspace），而非 cd 段。
    #[test]
    fn test_real_moved_dir_cd_cmds_within_scope() {
        let ws = std::path::PathBuf::from(r"E:\AIAD_Group\20260807-Agent科普");
        if !ws.exists() {
            return; // 目录不存在则跳过（非本机时）
        }
        for real in [
            "cd \"E:/AIAD_Group/20260807-Agent科普\" && officecli create \"AI Agent科普_v2.pptx\" 2>&1",
            "cd \"E:/AIAD_Group/20260807-Agent科普\" && officecli get \"AI Agent科普.pptx\" / --json 2>&1 | head -30",
            "cd \"E:/AIAD_Group/20260807-Agent科普\" && ls -la",
        ] {
            let within =
                tool_within_workspace("terminal", &json!({ "command": real }), &ws, &[]);
            assert!(within, "moved 目录内的真实命令应免审批:\n{real}");
        }
    }

    // 回归：带引号 + 正斜杠的 `cd "ws" && ...`，cd 目标即 workspace 本身，应免审批。
    #[test]
    fn test_repro_quoted_cd_cmd_within_moved_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ws = dir.path();
        // 等价于 `cd "E:/AIAD_Group/20260807-Agent科普" && ls -la`
        let quoted = ws.to_string_lossy().replace('\\', "/");
        let cmd = format!("cd \"{}\" && ls -la", quoted);
        let within = tool_within_workspace("terminal", &json!({ "command": cmd }), ws, &[]);
        assert!(
            within,
            "moved 目录内带引号的 cd 命令应判定为 workspace 内:\n{cmd}"
        );
    }
}
