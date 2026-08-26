use crate::agent::approval::{format_move_prompt, validate_move_target, PendingKind};
use crate::agent::Agent;
use crate::agent::AgentRegistry;
use crate::config::Config;
use crate::goal::{read_goal, set_goal, update_status, GoalStatus};
use crate::memory::markdown::compress_memory;
use anyhow::Result;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

pub enum SlashOutcome {
    Handled(String),
    Exit,
    NotSlash,
    /// 解决待定审批后，启动一轮 continuation turn 让模型基于结果继续。
    /// `notice` 立即呈现给用户（如工具结果摘要），`message` 作为 continuation 的用户消息喂给模型。
    Resume {
        notice: String,
        message: String,
    },
}

/// 处理斜杠命令，返回 (outcome, 输出文本)。
/// 输出文本由调用方决定如何呈现（CLI 打印，QQ 发回用户）。
/// `registry` 用于后台委派管理（/delegate-list / /delegate-cancel），无则相关命令提示缺失。
pub async fn try_handle(
    line: &str,
    agent: &mut Agent,
    registry: Option<Arc<AgentRegistry>>,
) -> Result<SlashOutcome> {
    let trimmed = line.trim();
    if !trimmed.starts_with('/') {
        return Ok(SlashOutcome::NotSlash);
    }
    let (cmd, args) = match trimmed.split_once(' ') {
        Some((c, a)) => (c, a.trim()),
        None => (trimmed, ""),
    };
    match cmd {
        "/exit" | "/quit" => Ok(SlashOutcome::Exit),
        "/help" => Ok(SlashOutcome::Handled(
            "commands: /new /exit /stop /compact /memory-compact /clear /stats /remember <text> /provider /permission [read-only|default|yolo] /reasoning [on|off] /skill list [--all] /ok <id> /deny <id> /answer <id> <text> /cancel <id> /move [<path>|home] (alias /cd) — no arg or `/move home` restores the home workspace /config /dream /dream-rollback /goal <text> /goal-list /goal-done /goal-cancel /env /migrate-secrets /delegate-list /delegate-cancel <id> /help"
                .into(),
        )),
        "/permission" => {
            if args.is_empty() {
                let cur = agent.permission_profile.read().await.clone();
                return Ok(SlashOutcome::Handled(format!(
                    "current permission profile: {}\nusage: /permission <read-only|default|yolo>",
                    cur
                )));
            }
            let p = args.to_lowercase();
            if !matches!(p.as_str(), "read-only" | "default" | "yolo") {
                return Ok(SlashOutcome::Handled(
                    "invalid profile, use one of: read-only | default | yolo".into(),
                ));
            }
            agent.set_permission_profile(&p).await;
            Ok(SlashOutcome::Handled(format!("[permission profile set to {}]", p)))
        }
        "/ok" | "/deny" => {
            let mut id_arg = args.trim().to_string();
            if id_arg.is_empty() {
                // 无 id（裸 /ok 或 /deny）：绝大多数时候交互式频道一次只有一条待审批，
                // 自动选唯一的 Approval；多条或没有时给明确提示，不猜测。
                let pendings: Vec<_> = agent
                    .approval_gate
                    .list()
                    .await
                    .into_iter()
                    .filter(|p| p.kind == PendingKind::Approval)
                    .collect();
                match pendings.len() {
                    0 => {
                        return Ok(SlashOutcome::Handled(
                            "[没有待审批的操作]（有多个时请用 /ok <id> 或 /deny <id> 指定）".into(),
                        ))
                    }
                    1 => id_arg = pendings[0].id.clone(),
                    n => {
                        return Ok(SlashOutcome::Handled(format!(
                            "有 {} 条待审批，请用 /ok <id> 或 /deny <id> 指定：{}",
                            n,
                            pendings
                                .iter()
                                .map(|p| p.id.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )))
                    }
                }
            }
            let approve = cmd == "/ok";
            match resolve_approval(agent, &id_arg, approve).await {
                Ok(Some(ApprovalOutcome::Resume { notice, message })) => {
                    Ok(SlashOutcome::Resume { notice, message })
                }
                Ok(Some(ApprovalOutcome::Done { notice })) => Ok(SlashOutcome::Handled(notice)),
                Ok(None) => Ok(SlashOutcome::Handled(format!(
                    "[{}] no pending approval {}",
                    cmd, args
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[{} failed: {}]", cmd, e))),
            }
        }
        "/answer" => {
            if args.is_empty() {
                return Ok(SlashOutcome::Handled(
                    "usage: /answer <id> <answer>".into(),
                ));
            }
            let (id, text) = match args.split_once(' ') {
                Some((i, t)) if !t.trim().is_empty() => (i.trim(), t.trim().to_string()),
                _ => {
                    return Ok(SlashOutcome::Handled(
                        "usage: /answer <id> <answer>  (multiple pending questions require the id)".into(),
                    ))
                }
            };
            match resolve_question(agent, id, &text).await {
                Ok(Some((notice, message))) => Ok(SlashOutcome::Resume { notice, message }),
                Ok(None) => Ok(SlashOutcome::Handled(format!(
                    "[/answer] no pending question {}",
                    id
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[answer failed: {}]", e))),
            }
        }
        "/cancel" => {
            if args.is_empty() {
                return Ok(SlashOutcome::Handled("usage: /cancel <id>".into()));
            }
            let id = args.trim();
            // 先尝试取消 pending question，再尝试审批
            if let Some(q) = agent.approval_gate.take_question(id).await {
                return Ok(SlashOutcome::Handled(format!(
                    "[cancelled question {}] {}",
                    id, q.question
                )));
            }
            if agent.approval_gate.take(id).await.is_some() {
                return Ok(SlashOutcome::Handled(format!("[cancelled approval {}]", id)));
            }
            Ok(SlashOutcome::Handled(format!("[/cancel] no pending {}", id)))
        }
        "/move" | "/cd" => {
            let arg = args.trim();
            // 无参数 / home / ~ / - ：快速恢复到原始（家目录）workspace，无需审批
            if arg.is_empty() || arg == "home" || arg == "~" || arg == "-" {
                let home = agent.workspace.clone();
                let current = agent.workspace_root.read().await.clone();
                if current == home {
                    return Ok(SlashOutcome::Handled(format!(
                        "workspace is already the home directory: {}",
                        home.display()
                    )));
                }
                agent.set_workspace(home.clone()).await;
                return Ok(SlashOutcome::Handled(format!(
                    "[restored to home workspace] {} (was: {})",
                    home.display(),
                    current.display()
                )));
            }
            match validate_move_target(args) {
                Ok(target) => {
                    let id = agent
                        .approval_gate
                        .register(
                            "__move_workspace",
                            &json!({ "path": target.to_string_lossy() }),
                            "move",
                            "cli",
                            &agent.alias,
                            false,
                        )
                        .await;
                    let prompt = format_move_prompt(&target, &id);
                    Ok(SlashOutcome::Handled(format!(
                        "[switch requested] reply `/ok {}` to confirm or `/deny {}` to cancel. {}",
                        id, id, prompt
                    )))
                }
                Err(e) => Ok(SlashOutcome::Handled(format!("[move failed: {}]", e))),
            }
        }
        "/new" => {
            // 真正开启一个新会话：新建 session 并切换到它（沿用当前会话的 channel），
            // 而非仅清空内存 context。否则所有"新"对话都会继续追加到同一个旧会话里。
            let channel = agent
                .session_store
                .channel_of(agent.session_id)?
                .unwrap_or_else(|| "cli".to_string());
            let new_id = agent
                .session_store
                .create_session(&Uuid::new_v4().to_string(), &channel)?;
            agent.session_id = new_id;
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[new session]".into()))
        }
        "/clear" => {
            agent.context.clear();
            agent.context.summary = None;
            Ok(SlashOutcome::Handled("[context cleared]".into()))
        }
        "/compact" => match agent.provider_for_compact().await {
            Some(p) => match agent.context.compact(p.as_ref(), 6, agent.context_size).await {
                Ok(_) => Ok(SlashOutcome::Handled("[compacted]".into())),
                Err(e) => Ok(SlashOutcome::Handled(format!("[compact failed: {}]", e))),
            },
            None => Ok(SlashOutcome::Handled(
                "[compact failed: provider not configured]".into(),
            )),
        },
        "/memory-compact" => {
            // 先 compact_provider，否则主 provider；两者皆无则降级报错（ADR-0025 显式持久化压缩路径）
            let provider = match agent.compact_provider_snapshot().await {
                Some(p) => p,
                None => match agent.provider_snapshot().await {
                    Some(p) => p,
                    None => {
                        return Ok(SlashOutcome::Handled(
                            "[memory-compact failed: no provider configured; set [provider.default] or runtime.compact_model first]"
                                .into(),
                        ))
                    }
                },
            };
            let memory_path = agent.workspace.join("MEMORY.md");
            let backup_dir = agent.workspace.join("backups");
            let tz = agent.timezone().await;
            match compress_memory(&memory_path, provider.as_ref(), &backup_dir, &tz).await {
                Ok(_) => Ok(SlashOutcome::Handled(
                    "[memory-compact] MEMORY.md compressed and saved (backup in workspace/backups/). Restart to apply in the running session.".into(),
                )),
                Err(e) => Ok(SlashOutcome::Handled(format!(
                    "[memory-compact failed: {}]",
                    e
                ))),
            }
        }
        "/stats" => {
            // 文本部分（system/summary/history/状态栏/todo/goal/env）+ tool definitions
            // （实际发送给 provider 的 tools JSON schema）合并估算，避免低估真实发送量。
            let tokens = agent.context.estimate_tokens();
            let tools_json = serde_json::to_string(&agent.tools.specs()).unwrap_or_default();
            let tools_tokens = tools_json.chars().count() / 4;
            let total = tokens + tools_tokens;
            let threshold_tokens = (agent.context_size as f64 * agent.context_threshold) as usize;
            let usage = if agent.context_size > 0 {
                (total as f64 / agent.context_size as f64 * 100.0) as u32
            } else {
                0
            };
            let summary_status = if agent.context.summary.is_some() {
                "yes"
            } else {
                "no"
            };
            // 工具分组计数：MCP 工具名为 `<server_id>__<tool_name>`，据此归类
            let tools_summary = {
                let mut builtin = 0usize;
                let mut mcp: HashMap<String, usize> = HashMap::new();
                for name in agent.tools.names() {
                    if let Some((server, _)) = name.split_once("__") {
                        *mcp.entry(server.to_string()).or_default() += 1;
                    } else {
                        builtin += 1;
                    }
                }
                let mcp_part = if mcp.is_empty() {
                    String::new()
                } else {
                    let mut servers: Vec<String> = mcp
                        .iter()
                        .map(|(s, n)| format!("{}: {}", s, n))
                        .collect();
                    servers.sort();
                    format!(" + {} mcp ({})", mcp.values().sum::<usize>(), servers.join(", "))
                };
                format!("{} builtin{}", builtin, mcp_part)
            };
            let info = format!(
                "context_size: {}\ncontext_threshold: {} ({} tokens)\n\
                 current tokens (est.): {} ({}% used, text {} + tools {})\n\
                 history msgs: {}
session_id: {}
summary: {}
tools: {}
\
                 compact_provider: {}",
                agent.context_size,
                agent.context_threshold,
                threshold_tokens,
                total,
                usage,
                tokens,
                tools_tokens,
                agent.context.history.len(),
                agent.session_id,
                summary_status,
                tools_summary,
                if agent.compact_provider_snapshot().await.is_some() {
                    "configured"
                } else {
                    "fallback to main"
                },
            );
            Ok(SlashOutcome::Handled(info))
        }
        "/remember" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled("usage: /remember <text>".into()))
            } else if let Some(tool) = agent.tools.get("memory_write") {
                let _ = tool
                    .execute(&serde_json::json!({"entry": args}), "cli")
                    .await;
                Ok(SlashOutcome::Handled("[remembered]".into()))
            } else {
                Ok(SlashOutcome::Handled(
                    "[memory_write tool not registered]".into(),
                ))
            }
        }
        "/provider" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled(list_providers(agent).await))
            } else {
                match switch_provider(agent, args).await {
                    Ok(msg) => Ok(SlashOutcome::Handled(msg)),
                    Err(e) => Ok(SlashOutcome::Handled(format!("[switch failed: {}]", e))),
                }
            }
        }
        "/config" => {
            let info = format!(
                "context_threshold: {}\nmax_iterations: {}\ncontext_size: {}\nhistory msgs: {}\nsummary: {}\ntools: {:?}",
                agent.context_threshold,
                agent.max_iterations,
                agent.context_size,
                agent.context.history.len(),
                agent.context.summary.is_some(),
                agent.tools.names()
            );
            Ok(SlashOutcome::Handled(info))
        }
        "/dream" => {
            // 手动触发一次做梦：两阶段记忆整理（跳过空闲门控）。
            match crate::cron::dream::run_dream(agent, "dream", 30, true).await {
                Ok(summary) => Ok(SlashOutcome::Handled(summary)),
                Err(e) => Ok(SlashOutcome::Handled(format!("[dream failed: {}]", e))),
            }
        }
        "/dream-rollback" => {
            // 回滚 MEMORY.md 到最近一份 .bak 备份。
            let memory_path = agent.workspace.join("MEMORY.md");
            let backup_dir = agent.workspace.join("MEMORY.backups");
            match crate::memory::dream::restore_memory(&memory_path, &backup_dir, None).await {
                Ok(restored) => Ok(SlashOutcome::Handled(format!(
                    "[dream rolled back to backup: {}]",
                    restored.display()
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[dream-rollback failed: {}]", e))),
            }
        }
        "/goal" => {
            // 设定/重置长期目标（覆盖式）。落盘到 agent 家目录 goal.md。
            if args.is_empty() {
                return Ok(SlashOutcome::Handled(
                    "usage: /goal <objective text> — set or reset the long-term goal".into(),
                ));
            }
            match set_goal(&agent.workspace, args) {
                Ok(p) => Ok(SlashOutcome::Handled(format!(
                    "[goal set] active. persisted to {}\nobjective: {}",
                    p.display(),
                    args
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!("[goal failed: {}]", e))),
            }
        }
        "/goal-list" => {
            // 只读展示当前目标状态。
            match read_goal(&agent.workspace) {
                Some(g) => {
                    let mut s = String::from("current goal:\n");
                    s.push_str(&format!("status: {}\n", g.status.as_str()));
                    if let Some(c) = &g.created_at {
                        s.push_str(&format!("created_at: {c}\n"));
                    }
                    if let Some(u) = &g.updated_at {
                        s.push_str(&format!("updated_at: {u}\n"));
                    }
                    s.push_str(&format!("objective: {}\n", g.objective));
                    s.push_str(&format!("progress: {}", g.progress));
                    Ok(SlashOutcome::Handled(s))
                }
                None => Ok(SlashOutcome::Handled(
                    "[no goal set] use /goal <text> to define a long-term objective".into(),
                )),
            }
        }
        "/goal-done" => {
            match update_status(&agent.workspace, GoalStatus::Done) {
                Ok(()) => Ok(SlashOutcome::Handled("[goal marked done]".into())),
                Err(e) => Ok(SlashOutcome::Handled(format!("[goal-done failed: {}]", e))),
            }
        }
        "/goal-cancel" => {
            match update_status(&agent.workspace, GoalStatus::Cancelled) {
                Ok(()) => Ok(SlashOutcome::Handled("[goal cancelled]".into())),
                Err(e) => Ok(SlashOutcome::Handled(format!("[goal-cancel failed: {}]", e))),
            }
        }
        "/skill" => {
            // 技能管理：/skill list [--all] 查看已加载技能及 active 状态
            let args = args.trim();
            if args == "list" || args == "list --all" {
                let skills_dir = agent.config_dir.join("skills");
                let skills = crate::skill::loader::load_skills(&skills_dir);
                let show_all = args.contains("--all");
                if skills.is_empty() {
                    return Ok(SlashOutcome::Handled("[no skills found] run `llaia init` to seed examples".into()));
                }
                let mut out = String::from("skills:\n");
                for s in &skills {
                    if !show_all && !s.active { continue; }
                    let mark = if s.active { "✓" } else { "✗" };
                    out.push_str(&format!("{} {} — {} (path: {})\n", mark, s.name, s.description, s.path.display()));
                }
                if !show_all {
                    out.push_str("\nuse `/skill list --all` to show inactive skills");
                }
                return Ok(SlashOutcome::Handled(out));
            }
            Ok(SlashOutcome::Handled("usage: /skill list [--all]".into()))
        }
        "/env" => {
            // 手动刷新环境探测（P5 E1）：重探本机工具链并更新注入文本。
            let env_text = crate::envprobe::probe().await;
            agent.context.env_state = (!env_text.is_empty()).then_some(env_text);
            let cur = agent
                .context
                .env_state
                .clone()
                .unwrap_or_else(|| "[env] (none detected)".into());
            Ok(SlashOutcome::Handled(format!(
                "[env refreshed] {}",
                cur
            )))
        }
        "/reasoning" => {
            // 会话级思考开关：/reasoning off 关深度思考（推理模型日常问答提速），
            // /reasoning on 恢复。仅对支持 chat_template_kwargs 的 provider 生效
            // （llama.cpp / Ollama / vLLM 等，其它端点忽略，无害）。
            let state = |a: &Agent| if a.thinking_off { "off" } else { "on" };
            match args.trim() {
                "off" => {
                    agent.thinking_off = true;
                    Ok(SlashOutcome::Handled("[reasoning: off]".into()))
                }
                "on" => {
                    agent.thinking_off = false;
                    Ok(SlashOutcome::Handled("[reasoning: on]".into()))
                }
                "" => Ok(SlashOutcome::Handled(format!(
                    "[reasoning: {}] usage: /reasoning on|off",
                    state(agent)
                ))),
                other => Ok(SlashOutcome::Handled(format!(
                    "[unknown arg '{}'] usage: /reasoning on|off",
                    other
                ))),
            }
        }
        "/migrate-secrets" => {
            // 敏感信息 .env 自动化（P5 S1）：把 config.toml 里的明文敏感字段
            // 迁移到 .env（config 改为 ${VAR} 引用），保留注释。
            let config_path = agent.config_dir.join("config.toml");
            match crate::config::secrets::migrate_config_secrets(&config_path) {
                Ok(0) => Ok(SlashOutcome::Handled(
                    "[no plaintext secrets found] config.toml already uses ${VAR} refs".into(),
                )),
                Ok(n) => Ok(SlashOutcome::Handled(format!(
                    "[migrated {} secret(s) to {}] config.toml now uses ${{VAR}} references; restart serve for env expansion",
                    n,
                    agent.config_dir.join(".env").display()
                ))),
                Err(e) => Ok(SlashOutcome::Handled(format!(
                    "[migrate-secrets failed: {}]",
                    e
                ))),
            }
        }
        "/delegate-list" => {
            match &registry {
                Some(reg) => {
                    let tasks = reg.background_tasks.lock().unwrap();
                    if tasks.is_empty() {
                        Ok(SlashOutcome::Handled("[no background delegate tasks]".into()))
                    } else {
                        let mut s = String::from("background delegate tasks:\n");
                        for t in tasks.values() {
                            let secs = t.started.elapsed().as_secs();
                            s.push_str(&format!(
                                "- {} [{}] running {}s\n",
                                t.id,
                                t.agent_name,
                                secs
                            ));
                        }
                        Ok(SlashOutcome::Handled(s))
                    }
                }
                None => Ok(SlashOutcome::Handled(
                    "[delegate-list] no registry in this environment".into(),
                )),
            }
        }
        "/delegate-cancel" => {
            if args.is_empty() {
                Ok(SlashOutcome::Handled("usage: /delegate-cancel <id>".into()))
            } else {
                match &registry {
                    Some(reg) => {
                        let mut tasks = reg.background_tasks.lock().unwrap();
                        match tasks.remove(args) {
                            Some(t) => {
                                drop(tasks);
                                t.handle.abort();
                                Ok(SlashOutcome::Handled(format!(
                                    "[cancelled background task {}]",
                                    args
                                )))
                            }
                            None => Ok(SlashOutcome::Handled(format!(
                                "[delegate-cancel] no such task {}",
                                args
                            ))),
                        }
                    }
                    None => Ok(SlashOutcome::Handled(
                        "[delegate-cancel] no registry in this environment".into(),
                    )),
                }
            }
        }
        _ => Ok(SlashOutcome::Handled(format!("[unknown command: {}]", cmd))),
    }
}

/// 审批解析结果：区分「需要 continuation turn」与「仅展示即可」。
enum ApprovalOutcome {
    /// 展示 notice，并把 message 作为用户消息喂给模型续跑（如普通工具执行结果）。
    Resume { notice: String, message: String },
    /// 仅展示 notice，不触发 continuation（如 /move 切换目录——纯环境操作，
    /// 模型无需参与，避免其基于旧上下文在新目录里自行开始干活）。
    Done { notice: String },
}

/// 解析一条待确认审批：从门控取出 pending，按批准/拒绝决定执行与否。
///
/// - 普通工具：批准则 `execute_with_events` 真正执行，拒绝则返回拒绝提示。
/// - `__move_workspace`：批准则把 agent 工作目录切到目标，拒绝则不动。
///
/// 返回 `Some(Resume)` 时，调用方应启动一次 continuation turn，
/// 把 `message` 作为用户消息喂给模型，让其基于工具结果继续。
/// `__move_workspace` 返回 `Done`（仅展示切换结果，不续跑）。
async fn resolve_approval(
    agent: &mut Agent,
    id: &str,
    approve: bool,
) -> Result<Option<ApprovalOutcome>> {
    let pending = agent.approval_gate.take(id).await;
    let pending = match pending {
        Some(p) => p,
        None => return Ok(None),
    };

    if pending.tool_name == "__move_workspace" {
        let notice = if approve {
            let target = validate_move_target(
                pending
                    .args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
            )?;
            agent.set_workspace(target.clone()).await;
            format!("[switched working directory to {}]", target.display())
        } else {
            "[denied] working directory unchanged".to_string()
        };
        // 只回显切换结果，不把消息喂给模型：/move 是用户主动的环境操作，
        // 模型无需基于它续跑（否则会带着旧上下文在新目录里自行开始干活）。
        return Ok(Some(ApprovalOutcome::Done { notice }));
    }

    let tool = match agent.tools.get(&pending.tool_name) {
        Some(t) => t.clone(),
        None => {
            return Ok(Some(ApprovalOutcome::Done {
                notice: format!("[tool not found: {}]", pending.tool_name),
            }))
        }
    };

    let result = if approve {
        match tool
            .execute_with_events(&pending.args, &pending.channel, None)
            .await
        {
            Ok(s) => s,
            Err(e) => format!("[error: {}]", e),
        }
    } else {
        // 拒绝：明确要求模型停止当前任务，而不是仅中性告知"被拒绝"。
        // 幂等提示词，防止模型换工具/换方案继续同一任务（见 /deny 反馈 bug）。
        format!(
            "用户拒绝了 `{}` 的执行。这是明确的停止信号：请立即停止当前任务，不要再执行该操作、替代方案或任何后续步骤。结束本轮并等待用户给出新的指示。",
            pending.tool_name
        )
    };

    let notice = format!(
        "[{}] {}",
        if approve { "approved" } else { "denied" },
        pending.tool_name
    );
    Ok(Some(ApprovalOutcome::Resume { notice, message: result }))
}

/// 解析一条待回答问题：取出 pending question，把 text 作为用户回答，
/// 返回 Resume 让模型基于答案继续（与审批 resume 同路径）。
async fn resolve_question(
    agent: &mut Agent,
    id: &str,
    text: &str,
) -> Result<Option<(String, String)>> {
    let q = match agent.approval_gate.take_question(id).await {
        Some(q) => q,
        None => return Ok(None),
    };
    let notice = format!("[answered {}] {}", id, q.question);
    let message = format!(
        "[用户对你刚才提出的问题给出了回答]\n问题：{}\n回答：{}",
        q.question, text
    );
    Ok(Some((notice, message)))
}

/// 把 config 中所有 provider/model 组合 flatten 成有序 model ref 列表
/// （provider id 排序，alias 排序），同时作为 `/provider <序号>` 的索引基准。
pub fn flatten_model_refs(config: &Config) -> Vec<String> {
    let mut ids: Vec<&String> = config.provider.keys().collect();
    ids.sort();
    let mut refs = Vec::new();
    for id in ids {
        let prov = &config.provider[id];
        let mut aliases: Vec<&String> = prov.model.keys().collect();
        aliases.sort();
        for alias in aliases {
            refs.push(format!("{}.{}", id, alias));
        }
    }
    refs
}

/// `/provider`：列出所有可用模型，当前模型标 `*`
async fn list_providers(agent: &Agent) -> String {
    let live_arc = agent.live_config();
    let live = live_arc.read().await;
    let refs = flatten_model_refs(&live);
    if refs.is_empty() {
        return "no providers configured".into();
    }
    let current_label = match agent.provider_snapshot().await {
        Some(p) => p.label(),
        None => String::new(),
    };
    let mut out = String::from("providers:\n");
    for (i, r) in refs.iter().enumerate() {
        // refs 由 flatten 生成，格式保证合法
        let (prov_id, alias) = Config::parse_model_ref(r).unwrap_or(("", ""));
        let model_name = live.provider[prov_id].model[alias].model.clone();
        let mark = if model_name == current_label {
            " *"
        } else {
            ""
        };
        out.push_str(&format!("{}. {} ({}){}\n", i + 1, r, model_name, mark));
    }
    out.push_str("usage: /provider <num> | /provider <id.alias>");
    out
}

/// `/provider <n>` 或 `/provider <id.alias>`：运行时切换，不写 config.toml
async fn switch_provider(agent: &mut Agent, arg: &str) -> Result<String> {
    let live_arc = agent.live_config();
    let live = live_arc.read().await;
    let refs = flatten_model_refs(&live);
    let model_ref = if let Ok(n) = arg.parse::<usize>() {
        refs.get(
            n.checked_sub(1)
                .ok_or_else(|| anyhow::anyhow!("index starts at 1"))?,
        )
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("index {} out of range (1-{})", n, refs.len()))?
    } else {
        arg.to_string()
    };
    // 走 build_provider_chain 而非 provider_from_ref：保留 [agent.<alias>].fallback 降级链，
    // 否则切换后 FallbackProvider 被裸替换丢失（回归见 test_provider_switch_preserves_fallback_chain）。
    let fallback = live
        .agent
        .get(&agent.alias)
        .map(|a| a.fallback.clone())
        .unwrap_or_default();
    let provider = crate::provider::build_provider_chain(&model_ref, &fallback, &live)?
        .ok_or_else(|| anyhow::anyhow!("provider unavailable"))?;
    agent.reload_provider(Some(provider)).await;
    tracing::info!(model = model_ref.as_str(), "provider switched at runtime");
    Ok(format!("[switched to {}]", model_ref))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::{AgentConfig, ModelConfig, ProviderConfig};
    use crate::memory::sqlite::SessionStore;
    use crate::provider::{ChatRequest, ChatResponse, Provider, StreamEvent};
    use async_trait::async_trait;
    use futures_util::stream::BoxStream;
    use std::sync::Arc;

    struct LabelProvider(String);

    #[async_trait]
    impl Provider for LabelProvider {
        async fn chat(&self, _req: &ChatRequest<'_>) -> Result<ChatResponse> {
            Ok(ChatResponse::default())
        }
        async fn chat_stream(&self, _req: &ChatRequest<'_>) -> BoxStream<'_, Result<StreamEvent>> {
            Box::pin(futures_util::stream::empty())
        }
        fn native_tool_calling(&self) -> bool {
            true
        }
        fn label(&self) -> String {
            self.0.clone()
        }
    }

    fn test_config() -> Config {
        let mut config = Config::default_for_workspace("/tmp/llaia-test");
        config.provider.insert(
            "b".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:8080/v1".into(),
                api_key: String::new(),
                compat: None,
                model: [(
                    "small".into(),
                    ModelConfig {
                        model: "small-model".into(),
                        native_tool_calling: true,
                        context_size: None,
                        max_tokens: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
        config.provider.insert(
            "a".into(),
            ProviderConfig {
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:8081/v1".into(),
                api_key: String::new(),
                compat: None,
                model: [(
                    "big".into(),
                    ModelConfig {
                        model: "big-model".into(),
                        native_tool_calling: true,
                        context_size: None,
                        max_tokens: None,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
        config.agent.insert(
            "main".into(),
            AgentConfig {
                model: "a.big".into(),
                soul: None,
                user: None,
                memory: None,
                denied_tools: vec![],
                delegate_timeout: 120,
                fallback: vec![],
                memory_token_budget: crate::config::default_memory_token_budget(),
            },
        );
        config
    }

    async fn test_agent(config: Config) -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        Agent::new(
            &config,
            Some(Arc::new(LabelProvider("big-model".into()))),
            None,
            None,
            Arc::new(crate::agent::runner::ToolRegistry::new()),
            Arc::new(store),
            sid,
            "sys".into(),
            8192,
            "/tmp/llaia-test/workspace".into(),
            std::sync::Arc::new(tokio::sync::RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            "/tmp/llaia-test".into(),
            true,
            "main".into(),
            None,
        )
        .await
    }

    #[test]
    fn test_flatten_model_refs_sorted() {
        // default_for_workspace 自带 default.qwen，加上测试插入的 a/b
        let refs = flatten_model_refs(&test_config());
        assert_eq!(refs, vec!["a.big", "b.small", "default.qwen"]);
    }

    #[tokio::test]
    async fn test_provider_list_marks_current() {
        let agent = test_agent(test_config()).await;
        let out = list_providers(&agent).await;
        assert!(out.contains("1. a.big (big-model) *"));
        assert!(out.contains("2. b.small (small-model)"));
        assert!(!out.contains("2. b.small (small-model) *"));
    }

    #[tokio::test]
    async fn test_provider_switch_by_index_and_ref() {
        let mut agent = test_agent(test_config()).await;

        // 按序号切换
        let msg = switch_provider(&mut agent, "2").await.unwrap();
        assert_eq!(msg, "[switched to b.small]");
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "small-model");

        // 按 ref 切换
        let msg = switch_provider(&mut agent, "a.big").await.unwrap();
        assert_eq!(msg, "[switched to a.big]");
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "big-model");
    }

    #[tokio::test]
    async fn test_provider_switch_invalid() {
        let mut agent = test_agent(test_config()).await;
        assert!(switch_provider(&mut agent, "9").await.is_err());
        assert!(switch_provider(&mut agent, "0").await.is_err());
        assert!(switch_provider(&mut agent, "nope.missing").await.is_err());
        // 切换失败不动现有 provider
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "big-model");
    }

    #[tokio::test]
    async fn test_provider_switch_preserves_fallback_chain() {
        // 回归：bug 版本 switch_provider 用 provider_from_ref 裸替换，
        // [agent.main].fallback 降级链被丢弃（kind 变回 "provider"）。
        let mut config = test_config();
        config.agent.get_mut("main").unwrap().fallback = vec!["b.small".into()];
        let mut agent = test_agent(config).await;
        let msg = switch_provider(&mut agent, "a.big").await.unwrap();
        assert_eq!(msg, "[switched to a.big]");
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.label(), "big-model");
        // 链必须保留：FallbackProvider kind == "fallback"
        assert_eq!(p.kind(), "fallback");
    }

    #[tokio::test]
    async fn test_provider_switch_without_fallback_is_bare() {
        // 未配置 fallback 时不包链（保持裸 provider，行为与旧版一致）
        let mut agent = test_agent(test_config()).await;
        switch_provider(&mut agent, "a.big").await.unwrap();
        let p = agent.provider_snapshot().await.unwrap();
        assert_eq!(p.kind(), "provider");
    }
}
