# agent-core 模块约束

## 模块定位

`crates/agent-core` 是与 UI 和应用生命周期无关的 Agent 执行引擎，负责一次 `AgentExecution` 内的模型调用、工具调用循环、上下文构建和执行事件。业务 Run 及其 `RunId` 属于 `assistant-runtime`。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- Agent Loop 与终止条件。
- resolved tool invocation 之后的类型化策略装配、Allow/Deny 授权闸和 Guardrail。
- 模型 Provider trait 和流式响应抽象。
- 单次 `AgentExecution` 的上下文组装、工具结果回填和事件输出。
- 每个 Model Step 建立请求前通过共享 Context Window Evaluator 执行上下文预检；
  判断只使用最近完整 Provider Result 的 `total_tokens` 和当前模型
  `context_window_tokens`。达到压缩阈值或 Provider 报告 Context Overflow 时，以
  可靠终态交回 Runtime。
- 与执行逻辑直接相关的 token、轮次和工具调用限制。
- 规范对话、Provider Codec、Safety/Recorder/Authorizer 等稳定能力接口。

Tool 抽象、注册表、派发器与文件/Shell 能力契约归 [`agent-tools`](agent-tools.md)；Core 只消费 `ToolSetSnapshot` 与派发结果。

Context Window Evaluator、历史布局和 replacement 校验归
[`agent-context`](agent-context.md)；Core 只调用共享能力，不复制实现。

## 核心约束

- Core 不知道 Tauri、窗口、会话列表、定时任务、SQLite 或配置文件位置。
- Core 不直接调用 `std::fs`、`tokio::fs`、`Command`、HTTP 工具或桌面 API；所有副作用通过注入的模型/工具 trait。
- Core 处理一个 `AgentExecution`，不持有 `RunId`，不负责跨会话排队和全局并发。
- `ExecutionSpec` 是已经由 Runtime 解析完成的不可变执行事实源；Core 不再维护 `AgentProfile`、配置默认值或覆盖顺序。
- `ExecutionSpec.model_request` 冻结 tool choice、generation、reasoning 和命名空间化
  Provider Options；Core 在每个 Model Step 原样克隆这四项，不按 Provider 名称注入默认值。
- `ExecutionSpec` 只消费冻结的完整 System Prompt；同一次执行的所有 Model Step 和压缩
  交接均复用该快照，Core 不读取或刷新 Pinned Memory Store。
- 执行必须可取消，模型流和工具调用都要观察取消信号；工具取消后 Core 等待 dispatch 完成资源清理，不直接丢弃 future；取消收敛前为批次内未结算调用补记 interrupted 错误 ToolResult，并原子完成 pending exchange。
- 资源预算使用显式 `Option`；Core 不注入隐藏的最大轮次、超时或输出限制；预算是副作用前硬边界（`max_steps` 模型调用前预检、`max_tool_calls` dispatch 前预检）。
- 启发式 Guardrail 检测器必须支持未配置、`Observe`、`Enforce`；未配置表示关闭，
  不能以未声明规则静默中止执行。
- 工具输入先完成 schema/类型校验和确定性参数解析，形成 resolved invocation 后才
  进入 Authorizer；授权和执行不得再次各自解析原始 JSON。
- Core 接收规范对话快照，不使用 `ConversationRef` 自行加载或持久化 Session。
- 规范对话记录与 UI/诊断事件分离；Provider 特有字段由 Codec 往返保真。
- Recorder 以 pending/completed 两阶段 tool exchange 表达副作用前写入与结果批次原子完成；
  `complete_tool_exchange` 只有在可靠提交成功后才能请求通用的上下文改变 continuation，规范快照不得
  暴露 pending exchange。
- Core 生成的 `ToolMessage.id` 使用 `ExchangeReceipt + 批内序号` 作为命名空间，不扫描当前
  Conversation。Recorder 必须保证同一规范 Conversation 中可共存的 exchange receipt 唯一且
  稳定；compact、窗口裁剪或新建 Engine 后仍不能复用历史 ToolMessage ID。
- 普通观察事件允许背压丢弃，唯一终态通过独立通道可靠交付；终态包括
  Completed、Failed、Cancelled、CompactionRequired 和 ContinuationRequired，均报告丢弃计数。
- CompactionRequired 同时携带 Engine 自身统计的已消费 Step 与实际 dispatch 工具数；这些值属于
  continuation 硬预算事实，Runtime 不得从可能丢弃的观察事件反推。
- Core 不发起上下文压缩请求、不生成 Context Checkpoint，也不在同一个
  `AgentExecution` 内压缩后重试；压缩编排和 continuation 属于上层。v0.3.0 由
  Runtime Harness 临时验证，正式 Runtime 接口留待总体设计。
- Provider 的 Context Overflow 通过 provider-neutral `ModelError` 进入 Core，Core
  转换为 Agent 层 CompactionRequired 终态；Provider 不直接发布 AgentEvent。
- 未完成 Model Step 的流式 delta 不是规范事实。Context Overflow 时丢弃当前 Step；
  Core 只有在完整 `TurnFinished` 后才记录 Tool Call 并执行工具，因此压缩交接不得
  留下未配对 Tool Call/Result。

## 最小可执行 Agent

- 空 `ToolSetSnapshot` 是合法输入：不含任何工具的 Agent 必须可以正常执行并纯文本收尾。
- Core 不内嵌任何工具实现，对标准工具壳或上层自定义工具无差别；
  工具与能力实现由 Runtime 或其他上层宿主装配。

## 权限与策略装配（Authorizer）

- 工具副作用顺序固定为 begin pending exchange → resolve batch → guardrail → authorize
  → execute → complete batch；resolve 不得访问真实文件系统、网络或启动进程。
- Core 提供策略顺序装配机制；策略可以明确 Allow/Deny 或未匹配后继续，全部未匹配
  才进入最终 Authorizer。Engine 本身只接收 Allow/Deny。
- `TypedPolicyAdapter<F, P>` 只在 facts 类型匹配时调用 typed policy；
  `ComposedToolAuthorizer` 严格按装配顺序采用第一个明确决策，全部 Continue
  才调用必传 final authorizer。
- whole-batch resolve 保留 valid/invalid 原位置；invalid item 直接结算错误
  `ToolResult`，不进入 policy、Authorizer、预算或 execute。
- 文件、Shell 和通用工具使用各自的类型化授权事实，不强制共享一个 whitelist
  结构；策略具体规则、Plan/Build 和名单内容由上层装配。
- Core 保证每个 Tool Call 独立过闸；普通工具仍按原顺序逐项授权和执行。只有一段连续、显式
  `ParallelEligible` 的 valid invocation 会同时等待授权，并在预算与 started 可靠边界完成后
  并发执行；Invalid 或 Serial 项构成顺序屏障，不跨越屏障重排副作用。
- 并行组结果必须按原 Tool Call 顺序观察 Guardrail、形成 ToolMessage 并原子完成 exchange；
  取消时先等待组内 Tool SPI 完成清理，再为未结算组统一形成 interrupted 结果。
- `ToolCompleted` 只能在 Recorder 可靠完成整批 exchange 后发布；Recorder/Store 失败不得先产生
  伪成功的工具完成事件，Core 随后收敛到 Failed。
- 审批编排（串行询问、攒批询问、规则自动放行）归 Runtime authorizer 实现；Core 在
  authorize 时提供本轮 resolved batch 上下文，批次审批交互由 Runtime 借此自行完成。
- Core 授权决策仅 `Allow` / `Deny { reason }`；`Deny` 在授权闸处转换为错误 `ToolResult`——回喂模型、驱动循环继续的唯一载体是 error `ToolResult`，对模型与循环不存在"被拒绝"类别；reason 措辞归 Runtime。
- `ExecutionContext.authorizer` 为必传字段，类型层面杜绝"无授权闸"的隐藏默认；Core 只提供显式装配用的 `AllowAllAuthorizer`。
- `Ask` 不进 Core 词汇表：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成，审批事件由 Runtime 自行产生。
- authorize 与取消存在 race，必须可断言；recorder 失败阻断后续副作用。

## 文件与 Shell 工具

文件/Shell 能力契约、resolved invocation 与标准工具壳定义见
[`agent-tools`](agent-tools.md)。Core 通过类型化策略和最终 Authorizer 完成决策编排；
真实文件/Shell 机制位于本地基础设施 Adapter，规则保存、用户交互、环境策略和持久
审计由 Runtime 完成。

## Memory 与 Provider 边界

- Core 不定义或调用 MemoryService、PinnedMemoryStore、MemoryRecall 或 RecallSource。
- Pinned Memory 在进入 Core 前已经成为冻结 System Prompt Part；Core 只机械传递快照。
- Pinned 修改和 Memory Recall 都通过普通 Tool Call/Result 工作，与其他标准工具无差别。
- Source 选择、协调、失败降级和 Store 持久化都属于工具能力实现及上层装配。
- Model Provider 一次只完成一个模型 Turn；工具继续循环属于 Agent Engine。
- endpoint、credential、model 和 context window 属于 ModelService 构造；每次请求的
  provider-neutral 语义配置属于 `ModelRequestConfig`，Provider 私有内容仍由 Adapter 校验。
- reasoning、tool call、tool result 和 Provider continuation state 必须进入规范对话；不能只依赖流式事件恢复。

## 不应放在本模块的内容

- Session 列表、标题、归档和持久化。
- 全局 Run 队列、定时任务、模型账户配置。
- `RunId` 的生成、状态、查询、取消、恢复和事件关联。
- Compression Strategy 执行、compression request、Context Checkpoint 持久化和
  上层 continuation 调度；v0.3.0 的对应行为只存在于 Runtime Harness。
- 真实文件权限、Shell 确认界面和审计存储。
- Tauri event/channel 与前端 DTO 转换。

## v0.10.0 任务退出边界

- 一个 engine task 驱动 Agent Loop；execution-owned observer 持有其 JoinHandle，并通过
  CompletionFuture 交付唯一结果。调用方 drop CompletionFuture 不取消执行或异常观察。
- engine panic/JoinError 映射为不含 panic payload 的 `ExecutionError::Internal`，同时尽力发送
  `ExecutionFailed` 终态事件；CompletionFuture 自身不得因内部 task 退出再次 panic。
- 该观察只收敛 Core 内部任务；Runtime Run 的状态、持久化和关闭兜底仍由 Runtime supervisor
  独立负责。

## Harness 验证

- 引擎 Harness 宿主在 `agent-testkit/tests/`（`agent-core` 被 testkit 依赖，不反向 dev-depend）。
- Agent Loop 使用 fake model 和 fake tool 覆盖：纯文本结束、工具调用、多轮工具、模型失败、工具失败和取消。
- 覆盖有限/无限预算与 Guardrail 三种模式，不假定固定最大轮次。
- 覆盖 Recorder begin/complete 失败与 pending 恢复、Authorizer Allow/Deny、授权等待中的
  取消 race、普通记忆工具多轮调用、工具取消清理完成和 Provider reasoning/tool-call
  往返保真。
- 覆盖当前执行投影中存在历史 ToolMessage 的后续调用，并断言新 ToolMessage 使用 exchange
  receipt 命名空间，不依赖遍历可见历史分配 ID。
- 事件顺序必须可断言，不依赖真实模型网络。
- 可运行效果演示位于 `crates/agent-testkit/examples/engine_demo.rs`。

```bash
cargo test -p agent-testkit
cargo clippy -p agent-core --all-targets --all-features -- -D warnings
```
