# 长任务机制参考（opencode 调研）

- 记录日期：260726
- 状态：待评估
- 来源：对 `~/github/opencode`（v1.18.3-dev, commit 08fb473735）的只读调研
- 目标版本：待定（预计为 Runtime 接入版本参考）

## 原始内容

调研问题：opencode 如何处理长任务？是否在一轮会话内完成？

核心结论：**opencode 没有任务拆分/调度系统**；长任务在同一 session 的 drain 循环里跑完，唯一拆分机制是 `task` 子代理工具（默认前台同步阻塞，后台模式为实验开关）。支撑长任务可持续的是一组配套设计：

1. **persist-first**：所有消息/part 变更先落盘（SQLite WAL，事件日志与投影同事务），drain 循环每轮重新从 DB 读全部消息（`packages/opencode/src/session/prompt.ts:1092`）。
2. **中断可落盘**：abort 时部分 text/reasoning 内容、未结算 tool call（标 `error + interrupted`）全部落盘，状态归 idle（`processor.ts:539-625`）。
3. **重启不自动恢复**：v1 无任何 re-drive；用户下一条消息驱动循环继续（退出条件天然支持"接着跑"）。v2 引擎（实验）有 durable input + 显式 resume，但崩溃自动恢复被作者明确标注为未实现。
4. **僵尸 tool call 读侧结算**：执行中崩溃遗留的 running tool call，在组装模型消息时合成错误 result（"[Tool execution was interrupted]"），保证 tool_use/result 配对（`session/message-v2.ts:349-360`；v2 在 drain 开头主动补发 `Tool.Failed`）。
5. **步数上限软收尾**：`agent.steps`（默认无限）到达时不硬停，注入 `MAX_STEPS_PROMPT` 让模型文本收尾；另有 doom_loop 同参检测（3 次）+ 上下文溢出自动 compaction；无时长/token/成本上限。
6. **排队语义**：同 session 连续发消息先落盘后 ensureRunning，运行中的循环下一轮自然捡起（v2 显式化为 `steer`（运行中注入）/`queue`（等 idle）两种 delivery）。
7. **BackgroundJob 刻意不持久**：作者注释 "Entries are intentionally not durable"，避免假装有重启语义；无 cron/定时任务机制。
8. **客户端附着**：v1 SSE 不重放（拉快照对齐）；v2 按 seq checkpoint 重放。处理循环独立于客户端连接运行。

## 背景与约束

对我们分层（agent-core 单次执行 / assistant-runtime 拥有 Run 与调度）的映射：

- opencode 的 drain 循环 ≈ 我们的 **Runtime Run 层**（处理会话待办直到清空）；它的单次 provider turn + 工具 ≈ 我们引擎内一轮。我们的 `AgentExecution`（一次输入一次执行）粒度比它的循环细，v0.2.0 引擎边界无需因此调整。
- Runtime 版本可参考的模式：僵尸 tool call 读侧结算（与"副作用前记录"互补）、durable input + steer/queue 排队（对应我们"同会话串行"）、显式 resume 而非自动恢复、SSE 快照对齐 + seq 重放、steps 软收尾（可作为预算到达时的策略选项，对比当前硬 `BudgetExceeded`）。
- BackgroundJob 的"刻意不持久"与我们"不假装有重启语义"的诚实边界一致。

## 补充：长任务中"疑问句断裂"的控制（2026-07-26 讨论结论）

问题：模型常在轮次末尾以疑问句收尾（"需要我继续吗？"），无人值守长任务中造成执行完成但任务未完成的断裂。结论：这是提示词纪律与执行模式问题，**不属于引擎机制**（引擎保持"无 tool call 即 Completed"的简单终止语义，禁止疑问句检测等隐式规则）。四层控制全部在 Runtime 层：

1. instructions 纪律（主防线）：无人值守模式提示词压制疑问句收尾、禁止请求许可、用合理默认继续、真阻塞时说明原因。
2. `ask_user`/`question` 类工具（参考 opencode 的 `question` 工具）：把"真正需要人拍板的提问"结构化为工具调用，挂起后由 Runtime 按有人/无人值守策略应答——与授权接缝（Ask 由 Runtime 代理）同构，引擎零改动。
3. Runtime 显式续跑：引擎 `Completed` ≠ 任务完成；Runtime 持任务目标，未达成时显式开新执行注入"继续"指令（对应 opencode drain 循环的退出语义）。
4. 目标可检验化：提示词中给出机器可判的完成判据（如"测试全绿"），配合第 3 层的续跑判定。

## 补充：长任务的核心约束——单轮撞上下文 limit（2026-07-26 讨论结论）

长任务问题的核心关注点是**单轮请求撞上上下文窗口**：每轮重发完整对话，对话单调增长，工具输出是膨胀主力，撞墙只是时间问题。这是路线图 v0.3.0（上下文系统）的主题本身。分层防线（按"越早越便宜"排序）：

1. 源头控制：工具输出有界（**v0.2.0 已覆盖**：read 分页/截断标记、search 条数上限、shell 尾部截断；opencode 另有超限落盘 + 预览提示，信息留在工具侧可再取回）。
2. 历史裁剪：旧工具输出替换为占位符，保留 Tool Call/Result 配对（v0.3.0）。
3. 压缩/摘要：早期历史压缩为 summary；不得破坏 reasoning、Provider 状态、配对（v0.3.0）。
4. token 预算与预估：组装前估算、超限前先裁剪/压缩（v0.3.0）。
5. Provider overflow 恢复：显式次数上限的压缩重试，禁止隐藏重试循环（v0.3.0）。

v0.2.0 预留的插入点：BuildingContext 显式步骤（v0.3.0 Prompt Compiler 的挂载点）；`ExecutionSpec` 未来引入的 `context` 字段（v0.3.0 预算策略落点）；规范投影 + journal 保证裁剪不碰事实源。

## 补充：单轮输出上限与"任务堆积在单轮"（2026-07-26 讨论结论）

关注点是**单轮输出 token 上限**（非上下文窗口）：宽泛任务目标下模型可能把大量工作堆积在一轮输出里，撞上输出上限被截断。危害分级：text 截断（`FinishReason::Length` 已规范保留）无害但难看；tool call arguments 截断危险，v0.1.0 行为为解析失败 → Protocol 受控失败，不静默执行半个调用。控制手段（全部引擎之外）：

1. 提示词纪律：限制单轮动作幅度（一次一个文件、edit 分块、先计划后执行）。
2. 工具形态：`edit`（字符串替换）把修改输出从整文件体积降为增量体积，是结构性缓解（v0.2.0 契约已含）；opencode 对 GPT 系的 `apply_patch` 同逻辑。
3. 目标分解物化：todo 工具（opencode `todowrite`）把分解写进对话逐项推进；或 Runtime plan-then-execute 编排；或子代理拆分。
4. 工程兜底：`finish_reason` 随 AssistantMessage 落 journal，Runtime 可区分"说完"与"被截断"并显式续跑；"Length + tool call 截断"可作为可恢复情形以"请分块"回喂（Runtime 策略层）。

三个长任务问题合观 = 轮次粒度管理：单轮不能太大（输出上限）、轮次间不能断（疑问句/续跑）、整体不能爆（上下文窗口）。

## 综合：宽泛目标的"多轮自动完成、用户无感"机制栈（2026-07-26 收口）

核心问题：用户给一个宽泛目标，系统自己拆成多轮跑完，全程无需介入。完整答案为四层机制栈：

1. **引擎内自驱循环（v0.2.0）**：模型经 tool call 自发分解任务，一轮输入自动跑出多轮 assistant/tool 交替。
2. **Runtime 的 Run 级 drain 循环（无感的核心）**：Runtime 持任务目标，执行结束评估"完成了吗"，未完成自动开新执行（快照承接 + 继续指令）；触发情形含 Length 截断续跑、上下文压缩续跑、疑问句收尾但判据未达成、重启后显式 resume。对应 opencode drain 循环退出语义（"最后一条 user 已被应答"）。
3. **完成判据可机器检验（无感前提）**：测试命令、todo 全勾选、模型按判据声明完成；宽泛目标先转成可检验形式（提示词纪律或 plan 执行）。
4. **硬性停止策略（安全边界）**：任务级次数/步数预算、无进展 Guardrail、连续失败上限、用户随时取消。

"无感"≠ 不打扰：权限审批、ask_user 决策分叉、不可恢复阻塞仍是结构化中断（Runtime 代理通知），区别于疑问句造成的流程死锁。

版本归属：L1 = v0.2.0；压缩续跑 = v0.3.0；审批/Guardrail = v0.4.0；L2/L3 主体与 ask_user/todo 装配 = Runtime 版本。

## 补充：同批多工具与轮次成本（2026-07-26 讨论结论）

- 一轮 ≠ 一次工具调用：一条 assistant message 可含 N 个 tool call（OpenAI/DeepSeek 协议原生支持，v0.1.0 codec 已按 index 聚合）；"Dispatcher 一次一个"是派发单元而非轮次粒度；v0.2.0 同批多工具为逐一过闸、顺序派发、结果一起回填（验收标准 1 已覆盖），轮内并行执行留后续显式策略（架构 §11）。
- 批量的真实约束是数据依赖：独立调用可同批（读取阶段大批），依赖前序结果的调用必须串行（修改阶段串行）；"独立调用同批发出"的提示词纪律归 Runtime instructions。
- 轮次成本：每轮重发对话是无状态请求的本质，缓解为 prompt caching（DeepSeek context caching；v0.1.0 `cached_input_tokens` 已打通观测）+ 缓存前缀稳定性（append-only 对话 + `ExecutionSpec` 不可变保证前缀逐轮稳定）——不可变执行规格除确定性外的另一收益。
- 节奏权衡：同批尽量多（省轮次）vs 单轮输出上限（防截断），由模型自行平衡，系统职责是修好批量通道与截断保护两个边界。
