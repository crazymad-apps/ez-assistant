# agent-model 模块约束

## 模块定位

`crates/agent-model` 定义 Provider-neutral 的单次模型 Turn 契约。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-types`](agent-types.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- `ModelService` 一次只调用一个 Provider Turn，不执行工具或自动继续 Agent Loop。
- 请求只携带语义输入；调用目标（endpoint、credential、model）归服务实例构造期参数，一个服务实例即一条编译完成的模型配置，请求与上下文不携带路由信息。
- `SystemPromptSnapshot` 保存有序、已完成渲染的最终 system parts，使用透明数组序列化；
  只提供整体构造、只读访问和消费，不提供原地修改接口。
- `ModelRequest.system` 必须使用冻结快照；调用方不能在单次 AgentExecution 的不同
  Model Step 中重新读取 Store 或动态改写该前缀。
- 规范事件具有明确 Part 生命周期和唯一终态。
- 请求和调用上下文不携带业务 `RunId`、应用 Session 或 credential 明文。
- 错误区分建立前失败、流中失败和取消，不泄露 prompt、credential 或完整响应正文。
- 建立错误提供 Connection、Timeout、RateLimited、Unavailable 与 Retry-After 等稳定事实，
  不通过展示文本解析重试资格；流中瞬态失败由显式重试装配决定是否恢复，最终仍只产生一个终态。
- `TraceContext` 只承载逻辑模型调用与 attempt 的诊断关联，不代表业务 Run，也不进入
  Provider HTTP header。
- `RetryingModelService` 对建流与流中瞬态错误共用显式有限 attempt 预算；流中断开
  释放旧流后从同一可靠请求恢复。`TurnRestarted` 只废弃未提交的响应，之后重新开始
  Part 生命周期；不允许重放工具。认证、配置、协议、上下文溢出及取消不进入瞬态重试。
- 瞬态重试等待必须响应取消，复用完全相同的 `ModelRequest`，并透明委托 capabilities 与
  context window 与独立输入限制；Core 不解释重试策略，只将 TurnRestarted 投影为当前 Step 重新开始并清除未提交 usage。
- `ModelRetryPolicy.with_tool_argument_corrections` 可显式启用流内 `ToolArguments` 反馈纠正，默认关闭。
  纠正只在有工具定义的请求中生效，次数上限同时受自身额度及共享 delays 总 attempt 预算约束；
  使用 `ContextInsertionPlan::request_only_internal` 给下一 attempt 追加一条有界解析错误说明，
  不修改冻结 system、不修补参数、不伪造 Tool Call/Result、不写回规范 Conversation。
  同一未提交响应中的工具均未执行，纠正提示要求重新生成尚未执行的调用，保留历史已完成结果；
  重复纠正替换临时反馈，不累积消息。编码请求阶段的坏参数、其他协议错误、取消和无工具旁路均不触发此路径。
- `ModelService` 直接暴露当前实例绑定模型的 `context_window_tokens`；调用方不得按
  provider/model 名称猜测窗口。`max_input_tokens()` 提供当前实例绑定模式的可选输入上限；
  未声明返回 None，不从窗口大小补值。
- Provider 明确报告上下文过长时使用 provider-neutral `ContextOverflow`；建立前与
  流内仍遵守既有错误边界。
- `GenerationConfig.max_output_tokens` 只表示本次请求的输出上限；是否能编码由具体
  Adapter 的方言配置判断，不在 `ModelCapabilities` 重复声明。
- `PreparedModelImages` 的附件路径与 Tool Image 相对路径使用两个独立请求级命名空间；
  `ModelImageResource::ToolImage` 携带的 Session 绝对根只服务当次预处理，不进入
  `ModelRequest`、Conversation、序列化或持久化。
- `ToolImageProjection` 是精确协议路由冻结事实，只允许 `Unsupported`、
  `NativeFunctionOutput` 和 `AggregatedUserInput`；运行时不得根据 Provider 错误或自然语言
  响应切换投影。`ToolChoiceCapabilities` 独立声明四种规范选择的支持集。
- 单次请求的图片上限按 Part 出现次数计算；预处理可以按来源身份去重，但不得因此放宽
  同一图片重复出现时的 Part 上限。
- 不依赖 Tauri、Runtime、数据库和具体 Provider Adapter。
- 不持久化 Trace；attempt observer 只报告原生事实，记录失败不能改变模型结果。

## v0.22.0 M0 单轮收集边界

- `collect_model_turn` 只收集一次已装配的 `ModelService` 调用，通过 `LifecycleValidator`
  强制执行事件生命周期，并返回最终 `AssistantMessage` 或规范 `ModelError`。
- helper 不选择模型、不构造业务输入、不设置超时，也不持有 Session、标题、压缩或其他领域语义；
  Rolling Summary 与后续旁路推理只能在调用方解释结果。

## 验证

```bash
cargo test -p agent-model
cargo clippy -p agent-model --all-targets --all-features -- -D warnings
```

## v0.29.0 F06 M1 宿主历史重建控制

`HistoryUnavailable` 表示 Adapter 已核实本次旧 ID 在建流前被明确拒绝；它不是通用瞬态重试原因。`ModelCallContext.rebuild_server_history` 是宿主本次调用的显式控制面指令，默认 false，不进入请求、事件或持久化；RetryingModelService 和其他装饰器必须原样转交。是否允许重建及其额度归宿主，不在 agent-model/Core 实现 Session、Journal 或工具重放。

## v0.29.0 F03 M1 过程事实

ReasoningDelta 每个增量携带中立 source；缺失按 raw 读取并省略 raw 序列化。LifecycleValidator 拒绝同一 part 来源变化或最终消息与已观察来源不一致，完整 ReasoningPart 仍随 TurnFinished 可靠交付。

## v0.30.0 F01 M1 唯一机制与辅助调用

- `ContextManagementMode` 是 owner 冻结的本地摘要/服务端压缩机制；`ContextPolicy` 是服务实例的冻结策略，原生阈值与状态作用域归实例，不能在请求里重新选择。普通 Debug 不展示作用域指纹。
- `ModelService.context_policy` 缺省本地摘要。可选 `count_input_tokens` 缺省返回 None，由宿主沿用既有 usage 与增量估算；可变调用上下文允许图片准备结果供后续调用复用。计量错误的降级编排在后续调用方接入。
- `compact` 是显式、单次、完整结果调用，缺省返回不支持或取消；不通过普通 stream 生成摘要、不执行工具。所有装饰器透明委托三项方法，RetryingModelService 不将辅助调用纳入普通生成重试或 attempt 观察。
- M1 不代表任何 Adapter 已实现原生 HTTP compact 或计量；实际协议接入归后续里程碑。
