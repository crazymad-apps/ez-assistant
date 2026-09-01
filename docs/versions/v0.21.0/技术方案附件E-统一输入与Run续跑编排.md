# v0.21.0 技术方案附件 E：统一输入与 Run 续跑编排

## 一、方案信息

- 版本：`v0.21.0`
- 状态：已确认（2026-08-31）；M9 已完成实现与验证，并于 2026-09-01 获得里程碑确认
- 影响模块：`apps/runtime-host`、`crates/assistant-runtime`、Runtime Store Adapter；
  `assistant-protocol` 只保留既有 Desktop 协议 DTO，不新增 continuation 协议类型
- 关联方案：[`v0.21.0 技术方案`](技术方案.md)、
  [`附件 B：Conversation 与回复路由`](技术方案附件B-Conversation与回复路由.md)、
  [`附件 C：语音与播报`](技术方案附件C-语音与播报.md)
- Review 输入：[`v0.21.0 临时修改意见`](临时修改意见.md)

本附件修订两条现有实现路径：

1. Desktop、Device 与未来 Channel 不再建立平行 Runtime 输入用例；
2. Tool/Skill、上下文压缩、Goal 推进和缺失播报不再各自建立续跑管道，而是在同一
   Run 编排流水线中按顺序充当门禁。

本方案不改变 Goal 的业务定义。Goal 仍独立拥有 objective、state、generation、turn、
预算、暂停/完成/恢复和代理报告回复路径。Goal 与续跑编排的唯一交叉点是 Goal 门禁：
Goal 领域先判断是否继续，需要继续时才转换为通用 `RunContinuation`。

## 二、目标与非目标

### 2.1 目标

- 以 Session 作为所有 Channel 输入的最大兼容实体，Runtime 只保留一个规范输入用例。
- 将“Device 默认进入当前主控”保留为 Host 产品路由策略，不固化为 Runtime 通用准入规则。
- 建立单一 Run 续跑流水线；一个产品 Run 可包含多个 `AgentExecution` Loop。
- 所有自动续跑保持相同 `RunId`、`active_run`、取消令牌、step 序列、执行预算和
  output cycle；不创建内部后继 Input、Run 或 Queue 项。
- 只在所有门禁全部放行后可靠结算 Run，并只发布一次 `RunFinished`。
- 使 Goal 保持独立领域状态机；通用 continuation 数据不携带 Goal 预算、恢复或路由事实。

### 2.2 非目标

- 不新增公共 crate、进程、消息总线、数据库表或第二份 Conversation。
- 不将 `RunContinuation` 暴露到 `assistant-protocol`、Desktop SSE 或 Device wire。
- 不让外部 wire payload 直接提交可信 Channel 身份。
- 不把 Goal 状态机、预算、回复路径或恢复规则塞入通用 continuation 类型。
- 不增加 `CurrentRun` / `NextRun` 分支；内部续跑的定义就是在当前 Run 中启动下一 Loop。
- 不在进程重启后自动重放未完成 Loop、TTS 或播放队列；中断 Run 和 Goal 继续按已有
  fail-closed 与显式恢复产品语义收敛。
- 不为本版本尚未发布的中间 Rust API、JSON 字段名或内部排队形状保留兼容别名、双读或
  迁移分支。

## 三、现状核查

### 3.1 输入用例分叉

当前 `AssistantRuntime::submit_input` 为 Desktop 固定补入 `Desktop { Text, Text }`，
`submit_session_channel_input` 另行承接 Device，两者最终都进入
`submit_input_with_source`。Runtime 还通过 `resolve_current_controller_channel_session` 和
`SessionRole::Controller` 把 Device 目标限制为当前主控。

这一实现将“可信来源准入”、“目标 Session 产品路由”和“规范 Input 接受”三个职责混合。
语音提交、文字提交以及 PTT 对活动 Run 的取消还分别直接查询当前 Controller，增加新
Channel 时会继续复制上述分叉。

### 3.2 Run 已有多 Loop 基线

Runtime Queue Driver 已能在同一 Run 内反复启动 `AgentExecution`，并在 Loop 之间累计
`remaining_budget` 与 `next_step`。Core 的 `ContinuationRequired { ContextChanged }` 与
`CompactionRequired` 已由这个外层循环消费。

因此本方案不重新搭建 Run 执行器，而是把现有循环收敛为唯一编排位置，并将 Goal 和缺失
播报从结算后管道迁入结算前门禁。

### 3.3 结算后 continuation 的额外生命周期

当前 Goal 自动推进和缺失播报都会创建新 `NewStoredInput`、Run 和 Queue 项。
`RunSettlementResult::continuation` 因此同时表示两种不同来源的后继 Run，外层再发布
`RunAccepted`。启动恢复还必须特别识别 queued Goal continuation 和 queued speech reminder。

这些结构没有增加业务事实，只是把“当前 Run 还没有真正结束”转译成了新的可靠输入、排队和
恢复生命周期。

## 四、概念与不变量

```text
Run
├─ Loop 1 / AgentExecution
│  ├─ Step 1 / Provider request
│  └─ Step 2 / Provider request + tools
├─ RunContinuation
├─ Loop 2 / AgentExecution
│  └─ Step 3 / Provider request
└─ RunSettlementResult
```

- **Step**：一次真实 Provider 请求，是 usage、流式事件和工具活动关联的最小模型单元。
- **Loop**：一个 `AgentExecution`，可内含多个 Step 和工具循环，直到产生一个
  `ExecutionOutcome`。
- **Run**：一次可靠接受的业务执行，拥有唯一 `RunId`、取消令牌、活动所有权和最终终态；
  可串联多个 Loop。
- **RunContinuation**：一个 Loop 结束后“当前 Run 必须再启动一个 Loop”的 Runtime 私有决定。
- **RunSettlementResult**：所有门禁都放行后，Store 已可靠提交 Run 终态的内部结果。

必须持续成立的不变量：

1. 一个 Input 只接受一个首次 Run；自动续跑不创建 Input、Run 或 Queue 项。
2. 同一 Run 的 Step 严格递增；续跑准备动作没有发起 Provider 请求时不消费 Step。
3. 同一 Run 的 step/tool 预算跨 Loop 累计，不因任何门禁重置。
4. `RunAccepted`、`RunStarted` 和 `RunFinished` 对每个 Run 各至多一次。
5. Run 续跑期间 `active_run` 始终指向同一 `RunId`，只在最终结算成功后清除。
6. 下一 Loop 总是重新读取已提交的权威 Journal；不通过 continuation 复制 Conversation 快照。
7. Goal 业务状态和 output cycle 仍有各自的权威所有者；`RunContinuation` 不成为第二份状态。

## 五、统一 Channel Input 方案

### 5.1 边界流程

```text
Desktop HTTP Command
→ Desktop Host Adapter
→ 固定注入 Desktop Channel 来源
┐
│
├→ 统一 Runtime submit_input
│   → 通用 Session/Input/Run 准入
│   → Store::accept_input
│
Device WSS / 未来 Channel
→ Channel Host Adapter 认证与能力校验
→ Host Channel Router 选择目标 Session
→ 注入可信 Channel 来源
┘
```

`assistant-protocol::SubmitInputRequest` 继续只是 Desktop wire DTO，不增加可由 WebView
自报的 `channel_source`。Host 在 `RuntimeCommand::SubmitInput` 分支中将它转换为 Runtime 可信输入；
Device Adapter 使用连接认证得到的 Device ID 构造同一输入。

统一 Runtime 请求需要表达目标 Session、可信 `InputChannelSource`、正文和已经 Channel Adapter
准入的选项。实现时应直接收敛现有 `SubmitInputRequest`、`SubmitSessionChannelInputRequest`
和 `submit_input_with_source` 的重复字段，只保留一个 Host 可调用的 Rust 用例边界。

该 Rust 请求是 Host 与 Runtime 之间的实际信任边界，具有独立的认证语义，因此可以作为
`assistant-runtime` 的轻量 Rust API；它不派生 `Serialize/Deserialize`，不进入
`assistant-protocol`。具体命名在实现时优先复用并重命名现有类型，不同时保留新旧两套入口。

### 5.2 Host Channel Router

Host 使用单一 `ChannelInputRouter` 或等价的私有函数选择目标 Session：

- Desktop 使用已通过 Desktop 应用协议提交的目标 Session；
- Device 在本版本仍选择当前产品主控 Session；
- 未来 Mobile、Web、Remote 或 IM 只增加 Host Adapter 和路由策略，不增加 Runtime 输入用例。

Router 每次从 Runtime 权威查询或快照选择目标，不持久 Session 列表、主控 ID 或第二份角色状态。
Device 文字、ASR 转录和 PTT 取消活动 Run 都必须经过这一 Router，避免只迁移提交却
保留取消特例。

### 5.3 Runtime 准入

Runtime 不再判断 Device 目标必须是 `SessionRole::Controller`，但仍必须校验：

- 目标 Session 存在、active、未故障、未压缩且可接受输入；
- Device 来源仍为 paired，Device ID 与 Host 注入的可信事实一致；
- Channel 的 modality、requested output、附件、Skill、Goal mode、variant 和幂等组合在该来源
  的能力范围内；
- Device `client_input_id` 继续派生确定性幂等身份，Desktop 继续使用现有幂等键；
- UserMessage、Input、首次 Run、Queue 和事件仍沿用一套可靠接受流程。

这些是通用业务不变量，不把 Controller 产品路由策略重新引入 Runtime。

## 六、统一 RunContinuation 方案

### 6.1 最小私有类型

现有各门禁在实际开始下一 Loop 前都必须先把上下文变化可靠提交到 Journal。因此下一
Loop 可统一重新读取权威 Journal，无需再把 `ConversationSnapshot`、`UserMessage` 或 Goal 领域事实
复制进 continuation。

目标类型保持为：

```rust
pub(crate) struct RunContinuation {
    pub(crate) reason: RunContinuationReason,
}

pub(crate) enum RunContinuationReason {
    ContextChanged,
    ContextCompacted,
    GoalProgress,
    SpeechReminder,
}
```

`ContextChanged` 继续承载 Recorder 已可靠提交的 Tool Image、Skill Activation 或其他上下文变化。
若后续诊断确实需要区分具体来源，使用私有调试 metadata，不先为每个工具新增流程分支。

本类型不需要：

- `CurrentRun` / `NextRun` scope；
- Input ID、Run ID、Queue position 或新的 revision；
- Goal state、budget、generation、reply route；
- Conversation 快照、模型正文或可序列化协议形状。

以上命名是方案口径，实现若能直接扩展现有 `ExecutionOutcome` 消费逻辑而不增加结构体，
可以保留等价的私有 enum。不得为了与文档代码块逐字一致而增加无消费者的包装层。

### 6.2 门禁流水线

```text
AgentExecution 返回 ExecutionOutcome
→ Gate 1：Core/Recorder ContextChanged
→ Gate 2：Context Compaction
→ Gate 3：Goal
→ Gate 4：Speech Reminder
→ 全部放行后 settle_run
```

每道门禁只有两种编排结果：

- **放行**：不需要续跑，把同一候选结果交给下一道门禁；
- **续跑**：先可靠完成该门禁所需的上下文/领域提交，再返回 `RunContinuation`；流水线
  本次立即结束，Run 编排器重新读取 Journal 并启动下一 Loop。

门禁不得返回“创建下一 Run”。如果流程已经构造 `RunSettlementResult`，就代表上述门禁全部
放行，不能再从结算结果派生 continuation。

### 6.3 各门禁语义

| 门禁 | 输入事实 | 续跑前的可靠动作 | 续跑原因 |
| --- | --- | --- | --- |
| ContextChanged | Core 返回 `ContinuationRequired` | Recorder 已提交 Tool Result/隐藏 Skill 上下文 | `ContextChanged` |
| Context Compaction | Core 返回 `CompactionRequired` | 通过既有 Context Replacement 提交压缩后 Journal | `ContextCompacted` |
| Goal | 候选结果、Goal latch 与 Goal 领域状态 | Goal 领域更新并提交必要的隐藏推进消息 | `GoalProgress` |
| Speech Reminder | 候选结果、output cycle 与实际渠道能力 | 提交候选 Assistant 消息与一次性隐藏播报提醒 | `SpeechReminder` |

ContextChanged 和 Compaction 只处理执行上下文。Goal 门禁只调用 Goal 领域判断，不把 Goal 状态
复制到 continuation。Speech 门禁始终最后执行，不得在 Goal 仍需自动推进时提前补播。

## 七、Goal 领域与续跑门禁的边界

### 7.1 Goal 领域继续拥有的事实

Goal 继续独立管理：

- objective 及其恢复校验；
- Running、Paused、Completed 状态与暂停原因；
- generation、turn、总 token/轮次/连续失败预算；
- `update_goal` latch 的 complete/blocked 业务信号；
- Stop、Resume、Clear、Fork 和启动恢复；
- ControllerDelivery 启动的 Goal 需要继承的 `reply_route`。

通用 Run 编排器不解释这些字段，也不以它们作为通用 continuation payload。

当前 `max_runs` / `used_runs` / `RunLimitReached` 的预算单位和命名属于 Goal 领域，不由本
continuation 方案重命名或重新定义。Goal 门禁只消费 Goal 领域已经得出的“继续/完成/暂停”决定；
现有预算字段不能反向要求续跑编排创建额外 Input/Run。若后续单独 Review Goal 领域后确认预算名称
与业务单位不一致，再以 Goal 方案单独收敛，不把该命名问题塞入 `RunContinuation`。

### 7.2 Goal 门禁

Goal 门禁只在前两道执行门禁放行后检查候选结果：

- 没有活动 Goal：直接放行；
- Goal complete：将 Goal 完成 effect 留给最终 Run 结算，本门禁放行；
- Goal blocked 或预算停止：将 Goal 暂停 effect 留给最终 Run 结算，本门禁放行；
- Goal 需要自动推进：Goal 领域计算并可靠提交 turn/预算变化、当前 Loop 的候选 Assistant
  消息和隐藏 Goal 推进消息，然后返回 `GoalProgress` continuation。

自动推进期间不创建 Goal Input、Run 或 Queue 项。这不改变 Goal 的业务状态机；只删除它曾经
自行维护的续跑传输管道。

### 7.3 显式恢复

Goal 暂停、等待用户或 Runtime 重启导致当前 Run 结束后，之后的用户显式 `resume_goal` 是新的
业务输入边界，可以接受新 Input 并创建新 Run。它不是自动 `RunContinuation`。

ControllerDelivery 启动的 Goal 在首次 Goal binding 中冻结 `reply_route`。同 Run 自动推进直接继续读取
该业务事实，不为每个 Loop 复制路由。显式恢复创建新 Input 时，由 Goal 恢复用例按已确认的
业务规则继承该路由；这仍是 Goal/跨会话业务，不进入 continuation 类型。

## 八、活动 Run 内的可靠提交

### 8.1 Store 业务接缝

Goal 和 Speech 门禁都可能在下一 Loop 前追加当前候选 Assistant 消息和隐藏 Runtime UserMessage。
这些消息必须在下一次 Provider 请求前进入权威 Conversation，但 Run 仍保持 running。

`RuntimeStore` 需要提供一个按业务原子操作表达的“活动 Run 内提交 continuation 上下文”接缝。
实现应复用现有 staged append/JSONL 恢复和 SQLite 事务语义，不新建 continuation 表或消息副本。

该操作必须原子校验和提交：

- 目标 Session/Input/Run 所有权一致，Run 仍是当前 running `active_run`；
- 当前 Loop 候选 Assistant Message 只提交一次；
- 可选隐藏 Runtime UserMessage 紧随当前候选消息；
- Goal 门禁需要时，在同一高层 Store 操作中提交 Goal 领域 effect；
- Run 状态保持 running，不清除 `active_run`，不改写 Input/Queue 生命周期；
- Store 成功前不修改内存 Journal、Goal 投影或 output cycle；
- Store 已成功但内存投影失败时 Session fail-closed，后续从权威存储恢复，不回滚已提交事实。

具体 DTO 和方法名在实现时先检查是否可以扩展既有 Conversation append 契约。只有现有契约无法
同时保证“消息顺序 + Goal effect + Run 保持活动”时，才增加单个新的 Store 业务方法。

### 8.2 恢复语义

- continuation 决定不持久，已提交的 Conversation 和 Goal 事实才是恢复来源。
- 进程在 Loop 之间崩溃时，该 Run 按已有未完成 Run 规则收敛为 Interrupted；不自动重放
  Provider 或工具副作用。
- 活动 Goal 按其独立恢复规则转为 `Paused(RecoveryRequired)`；用户显式恢复时才创建新
  Input/Run。
- 不再存在 queued Goal continuation 或 queued speech reminder，因此删除两类恢复特判。
- output cycle、Host TTS FIFO 和已生成未播放 PCM 继续为易失状态，重启后不补播。

## 九、缺失播报门禁

Speech Reminder 始终是最后一道门禁，只在以下条件同时成立时续跑：

- 当前 Loop 产生 `Completed` 候选结果；
- ContextChanged、Compaction 和 Goal 门禁都已放行；
- 当前路由在门禁执行时确实存在有效 audio delivery；
- output cycle 还没有成功 `speak`；
- output cycle 还没有发出过缺失播报提醒。

门禁可靠提交当前候选 Assistant Message 和带 `speech_delivery_reminder` 内部标识的隐藏
UserMessage，将 `speech_reminder_issued` 设为 true，然后在相同 Run 中启动下一 Loop。

第二个 Loop 不论是否成功 `speak` 都不得再发出提醒：

- 成功 `speak`：正常通过门禁；
- 仍未 `speak`：以明确的 `no_speak_text` 降级结束；
- Provider/工具失败或取消：按该 Run 的实际结果结算，不重放提醒。

已进入 Host FIFO 的 `speak` 片段可能已被用户听到，后续 Run 失败不回收这些易失副作用。

## 十、并发、取消与事件

- Session mutation gate 只覆盖输入接受、上下文/Goal 可靠提交和最终结算；不跨
  Provider 调用持有。
- Run supervisor 在所有 Loop 之间持有相同逻辑所有权、取消令牌、Recorder 和冻结 ToolSet。
- continuation 不重置工具集、审批方式、Agent Variant、model binding、step/tool 预算或输出周期。
- 中途取消作用于整个 Run，不只取消当前 Loop；被取消后不再进入后续门禁。
- `ExecutionCompleted`、`ExecutionContinuationRequired` 和 `ExecutionCompactionRequired` 是 Loop 观察事实，
  不是 Run 终态。
- Loop 之间可以发布 `ConversationCommitted`、`StepCommitted`、Goal 和工具事实；只有最终
  Store 结算成功后发布一次 `RunFinished`。
- Desktop 仍通过事件失效 + `get_session_view` 权威快照重建，不感知 Loop 序号或 continuation
  原因。

## 十一、模块落点与删除项

### 11.1 `apps/runtime-host`

- Desktop command adapter 把既有 `SubmitInputRequest` 归一化为 Desktop Channel 输入；
- Device Gateway 把已认证 Device 文字/ASR 结果归一化为同一 Runtime 输入；
- 建立一个 Host 私有 Channel Router，同时供输入提交和 PTT 取消目标解析使用；
- 删除 Host 对 Runtime `resolve_current_controller_channel_session` 的直接依赖。

### 11.2 `crates/assistant-runtime`

- 删除平行 `submit_session_channel_input` 及仅为入口桥接存在的 `submit_input_with_source`
  分层，收敛为单一规范输入用例；
- 删除“Device 目标必须是当前 Controller”的 Runtime 产品路由校验；
- 在 Queue Driver/Run supervisor 的现有多 `AgentExecution` 循环中建立单一门禁管道；
- 优先扩展现有 Conversation append/Store 契约，承载活动 Run 内隐藏消息可靠提交；
- 删除 Goal 自动推进的 `NewStoredInput`、新 Run、Queue 和 `StoredGoalSettlementEffect::Continue`；
- 删除缺失播报的 `NewStoredInput`、新 Run、Queue、`accept_input` 和启动恢复特判；
- 删除 `RunSettlementResult::continuation`，将 `committed_step` 改为语义准确的
  `final_message_step`；
- 删除只为跨后继 Run 传递 output cycle 或候选正文而存在的易失包装状态；仍保留
  `has_speech`、分段上限和一次性提醒等真实周期事实。

### 11.3 Store Adapter

- 复用 staged append 和现有事务 worker，不新增 continuation 表、序号或独立恢复队列；
- 删除 queued Goal/speech continuation 的合法性校验和恢复分支；
- 活动 Run 内提交失败时遵循既有“Store 成功前不投影，Store 成功后投影失败则
  fail-closed”规则。

## 十二、兼容、迁移与排除方案

- `v0.21.0` 尚未正式发布，直接修改 Rust 公共导出、内部 Store DTO 和开发期持久形状；
  不为 `submit_session_channel_input`、Goal 后继 Input 或 speech reminder Input 保留兼容别名。
- Desktop 对外 `SubmitInputRequest` 的 wire 形状保持不变，因此不升级 Desktop 应用协议。
- Device protocol 1.0 不变；Host 内部路由和 Runtime 编排改动不改变终端 wire。
- 不为开发期 SQLite/JSONL 数据自动增加兼容代码。如后续需要对实际 Runtime Home 执行
  破坏性清理、覆盖或迁移，必须按数据库安全规则另行核对和确认；本技术方案不授权任何
  实际数据操作。

否决以下方案：

- 为 Desktop、Device、Mobile 分别保留 Runtime 输入方法；
- 将可信 Channel 来源字段直接加入外部 wire DTO；
- 为 Goal 或 Speech 保留独立 continuation Queue；
- 增加 `ContinuationBoundary::{CurrentRun, NextRun}` 或等价分支；
- 持久化 continuation 决定、原因历史、Loop ID 或新 revision；
- 在 Host 复制 Session/Run/Goal 权威状态；
- 通过截断完整 Assistant 文本替代模型 `speak`。

## 十三、验证方案

### 13.1 Runtime 输入

- Desktop Adapter 和 Device Adapter 进入同一 Runtime 用例，并产生各自正确的可信来源。
- Device 来源不能伪造 ID，被 revoke 后即使 Host 连接尚未关闭也会被 Runtime 拒绝。
- Runtime 可接受不同合法 Channel 目标的 Session Input，不强制 `SessionRole::Controller`。
- Host 本版本仍将 Device 输入和 PTT 取消路由到当前主控，产品行为不变。
- Desktop/Device 幂等重试均只产生一份 Input/Run/Conversation 事实。

### 13.2 Run 续跑

- 单独与交错覆盖 ContextChanged、Compaction、GoalProgress 和 SpeechReminder；
- 任意数量 Loop 只有一个 `RunId`，并只发布一组 Run 生命周期事件；
- 同 Run Step 严格递增，step/tool 预算在所有 Loop 间累计；
- 下一 Loop 只能读到 Store 已成功提交的上下文，Store 失败不得继续请求 Provider；
- 取消可在任意 Loop 或门禁阶段结束整个 Run，不留下新的 queued Input。

### 13.3 Goal

- Goal 自动推进三次仍只有一个 Run，但 Goal turn、预算和 GoalChanged 事实按领域规则变化；
- Goal complete、blocked、预算暂停分别形成正确的 Goal 状态和唯一 Run 终态；
- 进程在 Goal Loop 之间崩溃后不重放，Goal 转为 `RecoveryRequired`，显式恢复才创建新 Run；
- ControllerDelivery 启动的 Goal 在自动推进中不丢失 `reply_route`，也不把它复制到
  `RunContinuation`。

### 13.4 Speech 与产品投影

- 需要 audio 但没有 `speak` 时，隐藏提醒保持同一 Run 且最多一次；
- Goal 还需推进时不提前触发缺失播报；
- 提醒后仍未 `speak` 时降级结束，不形成循环或第二 Run；
- Desktop 在多 Loop 期间仍能通过 Conversation/Step 事件展示已提交消息，只看到一个 Run 的
  running 到终态转换；
- `read_image` 的 request-only 图片信封、Conversation 事实和文件生命周期保持不变。

文档确认后，开发计划需增加一个独立的收敛里程碑，按“统一输入 → 抽取门禁流水线 → 活动
Run 提交接缝 → Goal 迁移 → Speech 迁移 → 删除旧管道与回归”的顺序实施；本附件不直接开始代码改动。
