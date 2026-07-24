# ADR-0004: 会话模型与上下文压缩

- 状态：Accepted
- 日期：2026-07-21

## 背景

LAIA 单用户但多频道（CLI/QQ/Web），需要明确"同一会话"的语义，
以及上下文超限时的压缩策略。

## 决策

### 会话身份

- 同一用户同一会话，跨频道接续
- "同一用户"由 USER.md 身份清单识别（见 ADR-0003）
- "同一会话"由 session_uuid 标识，跨频道共享上下文

### 会话切换

两种触发方式都有：

- 手动：`/new` 斜杠命令开新会话
- 自动：上下文占用超过阈值比例时自动压缩（见下）

### 上下文压缩

- 阈值：默认 70%，用户可配置（`context_threshold`）
- 策略：关键消息保留
  - SOUL/USER 永留
  - 首条用户消息留
  - 工具调用结果可丢
  - 其余旧消息被 LLM 摘要成一段替换
- 压缩后：旧消息从内存上下文移除，sqlite 留底不删
- 手动触发：`/compact` 斜杠命令
- 手动清空：`/clear` 清空当前内存上下文（sqlite 留底）

### sqlite schema（P1）

```sql
sessions:   id, session_uuid, channel, created_at, last_activity, token_count, state
messages:   id, session_id(FK), role, content, reasoning_content, created_at
tool_calls: id, message_id(FK), tool_call_id, tool_name, payload, outcome, created_at
```

参考 zeroclaw ACP schema，砍掉 P1 用不上的字段（workspace_dir、killed_at、agent_alias、session_events）。
P1 不做 FTS5 全文搜索，等 MEMORY 检索需要时再加。

## 影响

- 启动时需加载最近一个 session 的上下文（按 last_activity 排序）
- 跨频道共享上下文要求 session_uuid 全局可见，频道路由层只负责把消息塞进对应 session
- token 计数依赖 provider 返回的 usage 字段，无 usage 时回退到本地估算（P1 简单按字符数）

## 参考

- grilling 第二轮 Q8–Q9、第三轮 Q11、第四轮 Q19–Q20、Q27
- zeroclaw `crates/zeroclaw-infra/src/acp_session_store.rs`
