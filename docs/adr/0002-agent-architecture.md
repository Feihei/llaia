# ADR-0002: 主从 Agent 架构 = 委派模式（P2）

- 状态：Accepted
- 日期：2026-07-21

## 背景

README 写"主控 Agent + 多个专用 Agent 协作（类似 AstrBot）"，但 AstrBot 偏编排者模式，
而 LLAIA 的实际诉求是"用户只跟主 Agent 接触"。需要在三种模式中选定：

- (A) 编排者：主 Agent 拆任务、调度子 Agent 执行
- (B) 人格切换：同时只跑一个 Agent，"子 Agent"是不同提示词/技能包
- (C) 委派：主 Agent 把特定任务整体甩给子 Agent 独立完成

## 决策

采用 **委派模式（C）**，且 **P1 不实现，留到 P2 与 Web 面板一同上线**。

### P1（MVP）

- 只有主 Agent，单干所有任务
- 所有工具直接挂载在主 Agent 上，无工具白名单
- 不存在子 Agent 概念，不预留调度代码

### P2

- 用户通过 Web 面板创建预定义子 Agent（起名、写提示词、勾选工具白名单）
- 委派路由：混合策略——默认由主 Agent 的 LLM 判断，可被强制指令覆盖
- 子 Agent 起独立会话，主 Agent 传任务摘要过去；结果回传主 Agent 整合后再回用户
- 每个子 Agent 有工具白名单（coder 才能跑终端，searcher 不能）
- 委派失败/超时：主 Agent 有超时机制，超时后向用户报告"委派失败"，不重试不卡死

## 影响

- P1 代码不需要 dispatcher / sub-agent spawn / 上下文摘要传递逻辑
- P1 Provider 接口按"单会话"设计，P2 再扩展多会话编排
- 配置 schema 采用命名式 `[agent.<alias>]`，P1 只认 `main`，便于 P2 扩展
- 工具 trait 设计要考虑 P2 的白名单过滤，但 P1 不实现过滤逻辑

## 参考

- grilling 第二轮 Q5–Q7、Q13–Q15
- grilling 第三轮 Q12
