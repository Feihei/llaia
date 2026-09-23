//! Agent 实例层（ADR-0032）：任务线的运行时形态。
//!
//! serve 退化为宿主：`main` 也是实例；每条任务线 spawn 成一个独立实例
//! （独立 `Arc<Mutex<Agent>>`，进程内虚拟实例），频道经 per-channel 附着指针
//! 串行路由，WebUI 按面板自选实例并行。dormant 化 = 从注册表移除 handle，
//! Agent 随之 drop；session 线留在 sqlite，可随时重新 spawn。
//!
//! 两条计划级简化（对 ADR 字面的偏离，已注回 ADR）：
//! 1. SessionStore 共享不拆分——v1 进程内虚拟实例共享现有单个 `Mutex<Connection>`，
//!    进程内 Mutex 已串行化写入；独立连接 + busy_timeout 推迟到真进程载体。
//! 2. busy 判定免新增标志——回合期间 `Mutex<Agent>` 被持有（`run_turn` spawn 后
//!    lock agent 执行全程），`try_lock` 失败即 in-flight；成功后再查 gate 空。
//!    不引入 `turn_active: AtomicBool`。

use crate::agent::Agent;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// 实例类别：main 永驻；task 实例可 dormant 化。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstanceKind {
    Main,
    Task,
}

/// 实例句柄 = 一条任务线的运行时形态（ADR-0032）。
pub struct InstanceHandle {
    pub name: String,
    pub kind: InstanceKind,
    pub agent: Arc<Mutex<Agent>>,
    /// 该实例绑定的 session 线（spawn 时钉住；实例内 /session 语义归频道层管）
    pub session_id: i64,
    /// 任务线的绑定目录（sqlite `sessions.bound_path` 快照；None = 未绑定）
    pub bound_path: Option<PathBuf>,
    /// WebUI 面板订阅计数（T3/T7）：面板 open +1 / close -1。
    /// dormantize 要求计数为 0——防误杀仍被 WebUI 订阅的实例。
    pub subscribers: AtomicUsize,
}

impl InstanceHandle {
    /// idle = 无 in-flight 回合（agent Mutex 未被持有）且无未决 ask_user/审批。
    /// 频道内消息串行处理，无并发竞态。
    pub async fn is_idle(&self) -> bool {
        let Ok(agent) = self.agent.try_lock() else {
            return false;
        };
        agent.approval_gate.is_empty().await
    }

    pub fn subscribers_now(&self) -> usize {
        self.subscribers.load(Ordering::SeqCst)
    }
}

/// 宿主级实例注册表。attachments：IM/CLI 频道名 → 实例名（WebUI 不走此表，
/// 每面板自选实例并行）。main 实例由宿主（serve_cmd / chat）启动时注册。
pub struct InstanceRegistry {
    instances: RwLock<HashMap<String, Arc<InstanceHandle>>>,
    attachments: RwLock<HashMap<String, String>>,
}

impl Default for InstanceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InstanceRegistry {
    pub fn new() -> Self {
        Self {
            instances: RwLock::new(HashMap::new()),
            attachments: RwLock::new(HashMap::new()),
        }
    }

    /// 包装 main agent 为 Main 实例并注册（宿主启动时调用一次）。
    pub async fn register_main(&self, main: Arc<Mutex<Agent>>, session_id: i64) {
        let h = Arc::new(InstanceHandle {
            name: "main".into(),
            kind: InstanceKind::Main,
            agent: main,
            session_id,
            bound_path: None,
            subscribers: AtomicUsize::new(0),
        });
        self.register(h).await;
    }

    pub async fn register(&self, h: Arc<InstanceHandle>) {
        self.instances.write().await.insert(h.name.clone(), h);
    }

    pub async fn get(&self, name: &str) -> Option<Arc<InstanceHandle>> {
        self.instances.read().await.get(name).cloned()
    }

    /// 活跃实例列表（main 置顶）。
    pub async fn list(&self) -> Vec<Arc<InstanceHandle>> {
        let mut v: Vec<_> = self.instances.read().await.values().cloned().collect();
        v.sort_by_key(|h| h.kind != InstanceKind::Main);
        v
    }

    /// dormant 化（非 main、且 idle、且无 WebUI 订阅者）：移除 handle，Agent 随之 drop。
    /// session 线留 sqlite，之后可重新 spawn（回灌该线尾部）。
    pub async fn dormantize(&self, name: &str) -> Result<()> {
        let h = self
            .get(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("no such instance: {}", name))?;
        if h.kind == InstanceKind::Main {
            anyhow::bail!("main cannot dormantize");
        }
        if !h.is_idle().await {
            anyhow::bail!("instance \"{}\" is busy", name);
        }
        if h.subscribers_now() > 0 {
            anyhow::bail!(
                "instance \"{}\" has {} active webui subscriber(s)",
                name,
                h.subscribers_now()
            );
        }
        self.instances.write().await.remove(name);
        Ok(())
    }

    /// 频道附着：把某频道绑到某实例。默认 main；`/session` 切线时更新。
    pub async fn attach(&self, channel: &str, name: &str) -> Result<()> {
        if self.get(name).await.is_none() {
            anyhow::bail!("unknown instance: {}", name);
        }
        self.attachments
            .write()
            .await
            .insert(channel.to_string(), name.to_string());
        Ok(())
    }

    /// 频道当前附着的实例（无记录 = main；main 缺失时 panic 是宿主装配错误，fail fast）。
    pub async fn attached(&self, channel: &str) -> Arc<InstanceHandle> {
        let name = self
            .attachments
            .read()
            .await
            .get(channel)
            .cloned()
            .unwrap_or_else(|| "main".into());
        self.get(&name)
            .await
            .unwrap_or_else(|| panic!("attachment target missing: {}", name))
    }
}

/// 从 main agent 派生任务实例：fork 原语（共享 provider/store）+ 指定 session 线
/// + 回灌（复用 slash 切线同一通路）+ bound_path 对齐 WebUI 切线语义。
///
/// 目标线已注册为实例 → 直接复用句柄（幂等）；sqlite 无该线 → 新建任务线。
/// pin_root：先 pin 家目录（fork 原语默认），线有 bound_path 则切过去——
/// 与 WebUI 切线的「bound_dir 跟随」语义一致。
pub async fn spawn_task_instance(
    main: &Agent,
    registry: &InstanceRegistry,
    name: &str,
) -> Result<Arc<InstanceHandle>> {
    if let Some(h) = registry.get(name).await {
        return Ok(h);
    }
    let session_id = match main.session_store.find_open_task(name)? {
        Some(id) => id,
        None => {
            let channel = main
                .session_store
                .channel_of(main.session_id)?
                .unwrap_or_else(|| "web".to_string());
            main.session_store.create_task_session(
                &uuid::Uuid::new_v4().to_string(),
                &channel,
                name,
                None,
            )?
        }
    };
    let mut fork = main.fork_for_isolated(session_id, false, main.workspace.clone());
    fork.instance_name = Some(name.to_string());
    // bound_path：sqlite 读快照；有绑定则把 fork 作用域切过去（对齐 WebUI 切线语义）
    let bound = main
        .session_store
        .session_kind(session_id)?
        .and_then(|k| k.bound_path)
        .map(PathBuf::from);
    if let Some(p) = &bound {
        fork.set_workspace(p.clone()).await;
    }
    // 回灌该线尾部：与 /session 切线同预算同口径（slash::backfill_context）
    let msgs = main
        .session_store
        .recent_messages_within_budget(
            session_id,
            crate::commands::slash::TASK_BACKFILL_CHAR_BUDGET,
        )
        .unwrap_or_default();
    crate::commands::slash::backfill_context(&mut fork, msgs);
    fork.refresh_task_state().await;
    let h = Arc::new(InstanceHandle {
        name: name.to_string(),
        kind: InstanceKind::Task,
        agent: Arc::new(Mutex::new(fork)),
        session_id,
        bound_path: bound,
        subscribers: AtomicUsize::new(0),
    });
    registry.register(h.clone()).await;
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::memory::sqlite::SessionStore;
    use crate::provider::Role;

    /// 测试辅助：构造 main agent（无 provider 即可——实例层不跑 turn）。
    async fn make_main_agent() -> Agent {
        let store = SessionStore::open_in_memory().unwrap();
        let sid = store.create_session("test", "test").unwrap();
        let config = Config::default_for_workspace("/tmp/llaia-test");
        Agent::new(
            &config,
            None,
            None,
            None,
            Arc::new(crate::agent::runner::ToolRegistry::new()),
            Arc::new(store),
            sid,
            "test system".into(),
            8192,
            std::path::PathBuf::from("/tmp/llaia-test/workspace"),
            Arc::new(RwLock::new(std::path::PathBuf::from(
                "/tmp/llaia-test/workspace",
            ))),
            Arc::new(RwLock::new(Vec::new())),
            std::path::PathBuf::from("/tmp/llaia-test"),
            true,
            "main".into(),
            None,
        )
        .await
    }

    async fn test_instance() -> Arc<InstanceHandle> {
        let agent = make_main_agent().await;
        Arc::new(InstanceHandle {
            name: "t".into(),
            kind: InstanceKind::Task,
            agent: Arc::new(Mutex::new(agent)),
            session_id: 1,
            bound_path: None,
            subscribers: AtomicUsize::new(0),
        })
    }

    /// T1 Step 1：idle = Mutex 未被持有 且 gate 空。三者缺一不可。
    #[tokio::test]
    async fn test_instance_idle_requires_gate_empty() {
        let inst = test_instance().await;
        assert!(inst.is_idle().await);

        // gate 挂 pending question → 非 idle
        let gate = inst.agent.lock().await.approval_gate.clone();
        gate.register_question("q?", None, "cli", "main", 0).await;
        assert!(!inst.is_idle().await, "gate 非空必须非 idle");

        // 持有 agent 锁（模拟 in-flight 回合）→ 非 idle（try_lock 失败）
        let _g = inst.agent.lock().await;
        assert!(!inst.is_idle().await, "agent 锁被持有必须非 idle");
    }

    /// T1：dormantize 的三条门——main 不可、busy 不可、非 idle 的 gate 不可。
    #[tokio::test]
    async fn test_dormantize_guards() {
        let main_agent = make_main_agent().await;
        let main_sid = main_agent.session_id;
        let registry = InstanceRegistry::new();
        registry
            .register_main(Arc::new(Mutex::new(main_agent)), main_sid)
            .await;

        // main 不可 dormant
        assert!(registry.dormantize("main").await.is_err());

        // 未知实例
        assert!(registry.dormantize("ghost").await.is_err());

        // idle 的 task 实例可以 dormant；附着指向它时 attached 会 panic，
        // 所以 dormant 前应先换绑——这里只验证注册表行为。
        let inst = test_instance().await;
        registry.register(inst.clone()).await;
        registry
            .dormantize("t")
            .await
            .expect("idle task instance should dormantize");
        assert!(registry.get("t").await.is_none());

        // busy（gate 非空）→ 拒绝
        let inst2 = test_instance().await;
        registry.register(inst2.clone()).await;
        let gate = inst2.agent.lock().await.approval_gate.clone();
        gate.register_question("q?", None, "cli", "main", 0).await;
        assert!(
            registry.dormantize("t").await.is_err(),
            "busy instance must not dormantize"
        );
    }

    /// T1：attach/attached 换绑语义 + 未知实例拒绝。
    #[tokio::test]
    async fn test_attach_rebinds_channel() {
        let main_agent = make_main_agent().await;
        let main_sid = main_agent.session_id;
        let registry = InstanceRegistry::new();
        registry
            .register_main(Arc::new(Mutex::new(main_agent)), main_sid)
            .await;

        // 默认附 main
        assert_eq!(registry.attached("cli").await.name, "main");

        // 未知实例拒绝
        assert!(registry.attach("cli", "ghost").await.is_err());

        let inst = test_instance().await;
        registry.register(inst.clone()).await;
        registry.attach("cli", "t").await.unwrap();
        assert_eq!(registry.attached("cli").await.name, "t");
        // 其它频道不受影响
        assert_eq!(registry.attached("qq").await.name, "main");
    }

    /// T2 Step 1：spawn 任务实例——session 线复用、bound_path 跟随、
    /// 回灌非空、instance_name 标记。
    #[tokio::test]
    async fn test_spawn_task_instance_backfills_and_binds() {
        let agent = make_main_agent().await;
        let registry = InstanceRegistry::new();

        // sqlite 预置任务线：bound 到某目录 + 两条历史消息
        let task_id = agent
            .session_store
            .create_task_session("task-uuid-1", "web", "foo", Some("/tmp/llaia-test/bound"))
            .unwrap();
        agent
            .session_store
            .append_message(task_id, &Role::User, "earlier user msg")
            .unwrap();
        agent
            .session_store
            .append_message(task_id, &Role::Assistant, "earlier assistant msg")
            .unwrap();

        let h = spawn_task_instance(&agent, &registry, "foo").await.unwrap();
        assert_eq!(h.name, "foo");
        assert_eq!(h.kind, InstanceKind::Task);
        assert_eq!(h.session_id, task_id);
        assert_eq!(
            h.bound_path.as_deref(),
            Some(std::path::Path::new("/tmp/llaia-test/bound"))
        );

        let a = h.agent.lock().await;
        assert_eq!(
            a.instance_name.as_deref(),
            Some("foo"),
            "instance_name 必须标记"
        );
        assert_eq!(a.session_id, task_id);
        // 回灌：两条历史 + 一条边界标记
        assert!(
            a.context.history.len() >= 3,
            "context 尾部应有回灌内容，实际 {} 条",
            a.context.history.len()
        );
        assert!(
            a.context
                .history
                .iter()
                .any(|m| m.content.as_text().contains("earlier assistant msg")),
            "回灌应包含该线尾部正文"
        );
        // 作用域切到 bound_path（对齐 WebUI 切线的 bound_dir 跟随）
        assert_eq!(
            a.workspace_root.read().await.as_path(),
            std::path::Path::new("/tmp/llaia-test/bound")
        );
        // active_task 已刷新
        assert_eq!(
            a.active_task.as_ref().map(|t| t.title.as_str()),
            Some("foo")
        );
    }

    /// T2：sqlite 无该线时 spawn 新建任务线；重复 spawn 幂等复用同一句柄。
    #[tokio::test]
    async fn test_spawn_creates_missing_line_and_is_idempotent() {
        let agent = make_main_agent().await;
        let registry = InstanceRegistry::new();

        let h = spawn_task_instance(&agent, &registry, "brand-new")
            .await
            .unwrap();
        assert!(h.bound_path.is_none(), "新建线无绑定目录");
        let sid = h.session_id;
        // find_open_task 能找到新建的线
        assert_eq!(
            agent.session_store.find_open_task("brand-new").unwrap(),
            Some(sid)
        );

        // 幂等：同名再 spawn 返回同一句柄（同一 session）
        let h2 = spawn_task_instance(&agent, &registry, "brand-new")
            .await
            .unwrap();
        assert_eq!(h2.session_id, sid);
        assert!(Arc::ptr_eq(&h, &h2), "重复 spawn 必须复用同一实例句柄");
    }

    /// T2：fork 的谱系——task 实例的 Agent 与 main 共享 store 但 context 独立，
    /// main 的 context/session 不被 spawn 触碰。
    #[tokio::test]
    async fn test_spawn_does_not_touch_main() {
        let mut agent = make_main_agent().await;
        agent
            .context
            .push(crate::provider::ChatMessage::user("main history"));
        let main_history = agent.context.history.len();
        let registry = InstanceRegistry::new();
        spawn_task_instance(&agent, &registry, "iso").await.unwrap();
        assert_eq!(agent.context.history.len(), main_history);
        assert!(
            agent.instance_name.is_none(),
            "main 的 instance_name 不受影响"
        );
        assert!(agent.active_task.is_none(), "main 的 active_task 不受影响");
    }
}
