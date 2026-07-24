# ADR-0003: 持久化模型——三份 Markdown + sqlite 会话记录

- 状态：Accepted
- 日期：2026-07-21

## 背景

README 同时写了"持久化文件系统 SOUL/USER/MEMORY"和"记忆基于 sqlite 数据库"，
二者关系不明，需要澄清谁是 source of truth。

## 决策

### 三层持久化对象

| 对象 | 形态 | 用途 | 谁写 |
|---|---|---|---|
| SOUL.md | 单文件 Markdown | Agent 人格设定、行为准则、语气 | 用户编辑，启动加载 |
| USER.md | 单文件 Markdown | 用户画像、身份绑定清单、偏好 | 用户编辑 + Agent 写入偏好 |
| MEMORY.md | 单文件 Markdown，分条目 | 长期事实记忆 | Agent 写入（手动 /remember 或自动判断） |

### sqlite 角色

- 仅存 **会话记录**（messages、tool_calls），不存 SOUL/USER/MEMORY
- 是会话历史的 source of truth
- 上下文压缩时，旧消息从内存移除但 sqlite 留底

### MEMORY.md 结构

```markdown
- [2026-07-21] <条目内容>
- [2026-07-21] <条目内容>
```

### MEMORY.md 写入触发

- 手动：`/remember <text>` 斜杠命令
- 自动：主 Agent 的 LLM 自己判断该写入时直接写（靠提示词约束）

### MEMORY.md 膨胀处理

- 限定总 token 数上限
- 超限时：先备份当前 MEMORY.md，再由 LLM 去重、压缩后覆写

### USER.md 身份绑定

USER.md 列出 owner 在各频道的身份清单，任一频道命中即认作 owner：
```markdown
# 身份绑定
- qq: <openid>
- email: <addr>
- web: <username>
```

## 影响

- MEMORY 检索 P1 用全文搜索，P2 视需要再加向量索引
- sqlite schema 不需要 memory 表，只需要 sessions/messages/tool_calls
- 启动流程：读 SOUL.md + USER.md + MEMORY.md 拼入 system prompt

## 参考

- grilling 第一轮 Q3、第三轮 Q16–Q18、第四轮 Q20
- zeroclaw ACP session schema（参考但简化）
