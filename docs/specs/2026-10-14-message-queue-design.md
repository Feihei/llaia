# WebUI 消息队列设计（turn 进行中的 Send 排队 + 手动 Steer）

日期：2026-10-14

## 背景与问题

WebUI 聊天在 turn 进行中，发送按钮会变成 Steer，输入的消息立即加 `/steer` 前缀插进当前 turn。但用户存在「队列消息」需求——想说的内容应该等 turn 结束后作为普通消息发出去——此时按钮仍是 Steer，极易误操作：本想排队的消息被当成插话塞进了正在跑的回合。

## 目标

- turn 进行中，Send 按钮固定为 "Send"（不再切换标签）；点击后消息**入队**而非立即发出
- 队列消息显示在输入框上方（队列条），turn 正常结束后**自动发送**
- 队列项带 Steer 按钮，点击才真正作为插话插入当前 turn
- 输入框在队列占用时只作草稿，Send 置灰

非目标：跨标签页互斥、队列容量可配置、队列多条（每实例固定 1 条）、后端协议改动。

## 方案选型

| 方案 | 说明 | 结论 |
|---|---|---|
| **1. 纯前端队列（选定）** | Alpine `perQueue` 挂实例分桶，复用现有发送/steer/断线补发通路；localStorage 做刷新存活 | 零后端改动；队列本质是 UI 交互层（草稿语义） |
| 2. 后端队列 | web.rs 按实例挂 pending，turn 结束后自动投递 | 与「草稿」产品语义不符，改动大，否决 |
| 3. 前后端各半 | 两头各维护一份队列状态 | 复杂度最高，否决 |

## §1 数据结构与状态机

**数据**：

```js
perQueue: {},      // 实例名 → { text, images: [path...], ts }，每实例最多 1 条
// 派生：queue = perQueue[activeInstance] || null
// Send 置灰 = 队列占用 || 无内容（busy 只影响行为入口，不参与禁用判定）
```

输入框（草稿）保持**全局单份**（即现状），不按实例分桶——队列按实例分桶已满足隔离需求，草稿分桶会引入切面板时的草稿换入换出，徒增状态。因此**取回队列 = 写入当前活跃面板的输入框**，取回动作只在目标实例恰为当前面板时执行；后台实例的队列项等待用户切到该面板时再取回（切面板钩子里补查：该实例队列存在且 `!busy` → 立即取回）。

**生命周期**（每实例独立）：

```
                    Send（turn 进行中）
        ┌────────────────────────────────────┐
        ▼                                    │
   ┌─────────┐  done(无 Stop 意图)     ┌─────┴───┐
   │ QUEUED │ ───────────────────────► │ SENDING │──► 作为普通消息自动发出（带图）──► 清空
   └────┬────┘                         └─────────┘        │
        │ Steer（仅文本非空且无图）                          │ 失败/断线 → 回 QUEUED（localStorage 兜底）
        │ done(有 Stop 意图) → 取回输入框
        │ × 丢弃 / 点文字或 ↺ 取回草稿
        ▼
   输入框草稿（Send 恢复可用，可重新入队）
```

**done 归因（实现修订）**：`done` 事件本身不带原因，但 web 通道的三个终态事件——`done` / `interrupted` / `error`——类型即归因：Stop（按钮与 `/stop` 同路）终态发 `interrupted`，干净结束发 `done`。因此原方案的 `stopIntent` 标记整个不需要，`done` 分支按事件类型分发：`done` → 自动发送、`interrupted` → 取回输入框、`error` → 队列原样停住等用户处理。

**send() 行为矩阵（busy 下）**：

| 队列 | 输入内容 | Send | 行为 |
|---|---|---|---|
| 空 | 有文本/图 | 可用 | 入队（清空输入框，图从芯片区移入队列项） |
| 空 | 空 | 置灰 | — |
| 有 | 草稿 | **置灰** | 无操作 |

- `/stop` 输入直接执行，不受队列占用影响（Stop 永远可用），同时置位 stopIntent
- 空闲态行为完全不变（Send 即发、带图、`/btw`、`/steer` 旧通路）

## §2 UI 布局与交互

队列条插在 composer 上方（`upload-row` 同级兄弟），仅当前面板队列项非空时显示（正常路径下队列只在该实例 busy 期间存在）：

```
│ 消息流 …                                        │
├ 队列条：⟳ [图缩略×N] 文字预览… ✎/↺ ✕ [Steer?] ┤
├ composer：[+] [input（草稿）] [Send][Stop] ────┤
│ 上传芯片行（草稿的图）                           │
```

**队列条元素**（样式新增 `.queue-row`，沿用 upload-chip / session-rail-item 绿调）：

- **图片缩略**：最多 3 张 + `+N`；点 `×` 删该张；删光后 Steer 自动出现
- **文字预览**：单行 ellipsis + `title` 全文；**点击文字** = 取回草稿（图回芯片区、清队列项）
- **↺ 取回**：同点击文字的可见兜底
- **✕ 丢弃**：整体丢弃，队列腾空、Send 恢复
- **Steer 按钮**：仅 `queue.text.trim()` 非空 **且** `queue.images.length === 0` 时显示；点击发 `/steer <text>` 进当前 turn（复用现有 busy 发送通路）、清空队列、不回显对话流（回执由后端 Side 消息呈现）

**按钮/输入框态**：

- Send 标签固定 "Send"，`:disabled="队列占用 || (无文本 && 无图)"`
- placeholder 三态：`"Type a message..."`（空闲）/ `"Queued — edit your draft..."`（busy+占用）/ `"Type a message, it will queue..."`（busy+空闲）
- Stop 恒可用

- **auto-send 触发点**：`case 'done'/'interrupted'/'error'`（按实例落 `perBusy` 处）分发 `resolveQueueOnDone(inst, ev.type)`：`done` → 自动发送（与面板无关）；`interrupted` → 取回（仅当前面板写入输入框，否则留队列等切面板补查）；`error` → 队列不动。

取回与取回草稿合并规则：输入框已有草稿时追加换行合并（不丢字）。

## §3 边界情况

1. **刷新恢复**：`localStorage` 键 `llaia.msgQueue` = `{ [inst]: {text, images, ts} }`，`perQueue` 每次变更写入、启动读回。
   - 关键缺失：刷新丢 `perBusy`，恢复的队列永远等不到 done → **用服务端 busy 回填**：`loadInstances()` 拉 `/api/instances` 后同步 `perBusy[it.name] = it.busy`（现只用于 rail 徽标）。
   - 回填结果 `busy=false` 且队列非空 → **不自动发送**（done 已错过），直接取回输入框当草稿。规则：「turn 没在跑就不存在排队理由」。
2. **断线**：现有 onclose 清 `perBusy`、3s 重连；队列项不动。重连后 `loadInstances` 回填，turn 仍在跑（服务端不随 WS 终止）→ 照常等 done；turn 已在离线期间结束 → 走 1 的取回兜底。
3. **多标签页**：不监听 `storage` 事件、不做互斥——后写覆盖先写。双开同实例同时操作是边缘场景，localStorage 仅服务「刷新不丢」，单用户约定同时只开一个主标签页。
4. **Steer 发送瞬间断线**：队列项即刻清空，帧走现有 `outbox` 补发通路。
5. **实例消失**：`loadInstances` 结果与 `perQueue` 比对，孤儿键静默丢弃。
6. **自动发送失败（WS 未开）**：复用 `queueFrame` 断线补发，与现有行为一致。

## §4 测试点

**Rust 自动化**（沿用 `test_static_assets_embedded_up_to_date` 字符串断言，防嵌入资源过期）：

- `index.html` 含 `queue-row` 队列条标记与队列 Steer 按钮
- `app.js` 含 `perQueue`、`steerQueue`、`llaia.msgQueue`、`loadInstances` busy 回填标记

**手动验收清单**：

1. turn 中输入点 Send → 入队，队列条显示，Send 置灰，输入框留草稿
2. 队列项点 Steer → 插进当前 turn，队列清空，Send 恢复
3. 队列项带图 → 无 Steer；删完全部图 → Steer 出现
4. `×` 丢弃 / 点文字取回（图回芯片区）
5. turn 正常结束 → 队列消息自动发出（带图正常）、队列清空
6. turn 中 Stop → 队列消息回到输入框等手动 Send
7. 排队后刷新 → 队列恢复；服务端 turn 已结束时 → 取回输入框
8. 切实例 → 队列各自独立、切到「队列存在且已结束」的面板自动取回草稿；`/stop` 任何时候直接生效
9. 空闲态零回归（Send/图片/`/btw`/`/steer` 旧通路）

## 涉及文件

| 文件 | 改动 |
|---|---|
| `src/web/static/app.js` | `perQueue`/`stopIntent` 状态、send() busy 分支改入队、done 钩子、Steer/丢弃/取回方法、localStorage、loadInstances 回填 |
| `src/web/static/index.html` | composer 上方队列条、Send 禁用态与 placeholder 三态 |
| `src/web/static/theme.css` | `.queue-row` 样式 |
| `src/web/mod.rs` | 静态资源一致性断言追加 |

无后端协议改动；静态资源编译期嵌入，需 `cargo build` 生效。
