# Agent SDK 导读与候选 API 基线

## 一、文档定位

本文说明普通宿主如何使用 `agent-sdk`，以及什么时候应该绕过 Facade、直接依赖能力所属
crate。它同时保留由 v0.7.0 首次建立的候选公共 API 分层基线。

这份基线不是 crates.io 发布或 SemVer 稳定承诺。当前 workspace package version 仍保持
`0.1.0`；Core 公共面还需要经过正式 Runtime 和桌面应用集成后才能判断是否稳定。

审查依据包括各 Agent crate 的根导出、源码 `pub` 清单、workspace 反向依赖树、现有 Harness/
Demo/Adapter 调用点和 Rustdoc。首次候选审查结论是：现有底层公共面均有真实跨 crate 调用或
明确的替换边界，因此没有为形式收敛修改可见性或 serde 形状。

## 二、先区分三个对象

### `Agent`

`Agent` 是一个冻结的执行配置与启动 Facade：

- ModelService；
- 已渲染完成的 System Prompt；
- Context Window Evaluator；
- ToolSet；
- Model Request、Budget 与 Guardrail 配置。

它不包含 Conversation、SessionId、RunId、Journal、审批状态、Memory Store 或任务调度。
同一会话的上下文变更执行是否串行，由拥有 Journal 的上层保证。

### `AgentExecution`

`AgentExecution` 是一次正在运行的 Core 状态机，包含三个独立句柄：

- `events`：允许观察侧断开或因背压丢失普通事件；
- `completion`：可靠返回唯一 `ExecutionOutcome`；
- `control`：只取消本次执行。

一个 `Agent` 可以被同一会话顺序复用。不同会话应各自创建 Agent，但可以共享线程安全的
`Arc<dyn ModelService>`、Transport 和底层能力。

### `AgentEvent`

`AgentEvent` 用于 UI 流式展示、日志和指标，不是规范对话的权威事实源。事件消费者断开不会
取消执行；最终 AssistantMessage 从 `ExecutionOutcome::Completed` 获取。工具调用与结果的
权威落账通过 `ExecutionRecorder` 完成。

## 三、构建 Agent

普通入口是：

```rust
use std::sync::Arc;

use agent_sdk::{
    AgentBuilder, ContextWindowEvaluator, ModelRequestConfig, SystemPromptSnapshot,
};

let agent = AgentBuilder::new(
    model,
    SystemPromptSnapshot::new(vec!["You are a helpful assistant.".to_owned()]),
    Arc::new(ContextWindowEvaluator::new(0.8)?),
)
.tools(tools)
.model_request(ModelRequestConfig::default())
.build()?;
```

三个必填输入在 `new` 中显式出现。未覆盖的默认值为：空 ToolSet、默认模型请求、全 `None`
Budget 和关闭 Guardrail；SDK 不读取环境变量或注入隐藏轮次限制。

Builder 在调用模型前检查五类跨字段错误：

| 错误 | 含义 |
| --- | --- |
| `ZeroContextWindow` | 模型没有提供有效上下文窗口 |
| `ToolCallsUnsupported` | 已注册工具，但模型未声明 tool-call 能力 |
| `RequiredToolChoiceWithoutTools` | ToolChoice 要求调用工具，但工具集为空 |
| `NamedToolChoiceNotRegistered` | 指定工具不在冻结 ToolSet 中 |
| `ReasoningUnsupported` | 请求 reasoning，但模型未声明相应能力 |

generation 数值与 Provider Options 内容仍由具体 Adapter 校验。SDK 不按 Provider 名称添加
特判。

## 四、选择执行路径

### 临时、不可恢复路径

```rust
use std::sync::Arc;

use agent_sdk::AllowAllAuthorizer;
use tokio_util::sync::CancellationToken;

let execution = agent.start_ephemeral(
    input,
    CancellationToken::new(),
    Arc::new(AllowAllAuthorizer),
);
let outcome = execution.completion.await;
```

`start_ephemeral` 适用于 examples、一次性任务和不需要恢复的宿主。其私有 Recorder 不是
Noop：仍严格执行 `begin → complete`、拒绝 pending 重入并校验 receipt；结算后立即丢弃完整
消息，进程异常时没有恢复保证。Authorizer 仍然必传，全放行必须是调用方的显式选择。

### Runtime 风格的权威落账路径

```rust
use agent_sdk::ExecutionContext;

let execution = agent.start(
    input,
    ExecutionContext {
        cancellation,
        recorder,
        authorizer,
    },
);
```

正式 Runtime 应使用这条路径，将当前 Run 绑定的权威 Recorder 与审批 Authorizer 显式传入。
Recorder 的两阶段顺序是：

```text
完整 Assistant tool-call message
→ begin pending
→ 授权与工具副作用
→ 完整 ToolResult 批次
→ complete
```

begin 失败必须阻止副作用；complete 失败保留可恢复 pending。最终不含工具的 AssistantMessage
只从 Completion 返回，由上层写入 Journal。

## 五、事件与完成结果应并行消费

```rust
use futures_util::StreamExt;

let agent_sdk::AgentExecution {
    mut events,
    completion,
    control,
} = execution;

let event_task = tokio::spawn(async move {
    while let Some(event) = events.next().await {
        render_or_record(event);
    }
});

let outcome = completion.await;
event_task.await?;
```

示例中的 `render_or_record` 只是宿主伪代码。观察事件可以丢失，Completion 才是执行终态；
UI/SSE 消费速度不得反压 Provider。需要取消时调用 `control.cancel()`，不要取消共享父 Session
之外的其他执行。`Agent::start`、`start_ephemeral` 与底层 `AgentExecution::start` 都会创建
Tokio task，因此必须在可执行 `tokio::spawn` 的 Tokio Runtime 中调用。

## 六、会话初始化与具体能力装配

新建会话时，上层先读取当前配置与 Pinned Store，将最终指令渲染为完整
`SystemPromptSnapshot`，再选择 ModelService、ContextWindowEvaluator 和 ToolSet 构建 Agent。
Store 后续变化不修改已经构建的 Agent。

恢复会话时，上层必须读取持久化的最终 `SystemPromptSnapshot` 重建 Agent，不能用当前 Store
重新渲染历史会话。Conversation、Run、Journal、调度和恢复事务始终由上层持有，不进入 SDK。

使用真实能力时，装配顺序保持为：

```text
endpoint / credential / model / context window / Profile
→ OpenAiCompatibleService（按需包装有限建流前 Retry）
→ LocalFileSystem / LocalShell 或其他能力 Adapter
→ SessionPathResolver + 标准工具壳 + ToolRegistry Snapshot
→ SystemPromptSnapshot + ContextWindowEvaluator + ModelRequestConfig
→ AgentBuilder
→ 每次执行显式绑定 Recorder、Authorizer 与 CancellationToken
```

Provider 私有选项由宿主放进命名空间化 `ModelRequestConfig.provider_options`；SDK 不按模型名称
猜测 thinking、endpoint 或窗口。文件/Shell 的工作目录、环境策略和执行限制由具体 Adapter 与
上层策略决定，SDK 不把逻辑路径描述为系统沙箱。Core Demo 展示了完整组合结果，但其 HTTP、
Session、Journal、审批和 Store 实现不是可复制的 SDK 契约。

## 七、候选公共 API 分层

表中的“失败语义”概括该接口家族的主要错误边界；叶子请求/响应 DTO 归属相同家族，不逐个
重复列出。

### 7.1 普通入口：优先从 `agent-sdk` 导入

| 导入与类型 | 职责与所有权 | 失败语义 | 并发约束 |
| --- | --- | --- | --- |
| `agent_sdk::{AgentBuilder, Agent, AgentBuildError}` | 构建并保存冻结执行配置 | 构建期五类脱敏错误 | Agent 无内部 Session 锁；同会话串行由上层保证 |
| `AgentExecution`、`ExecutionOutcome` | 一次执行的事件、完成和取消句柄 | Completed / Failed / Cancelled / CompactionRequired 唯一终态 | 每次启动创建独立 Tokio task 与取消子令牌 |
| `ExecutionInput`、`ExecutionContext` | 完整规范输入与本次运行控制面 | Recorder/模型/Core 错误进入受控终态 | Context 只属于本次执行 |
| `ExecutionBudget`、`ModelRequestConfig` | 冻结预算和每 Step 请求配置 | 无隐藏上限；Adapter 校验 Provider 配置 | 启动时 Clone，执行中不变 |
| `SystemPromptSnapshot`、`ContextWindowEvaluator`、`ToolSetSnapshot` | 三类冻结能力输入 | 各自构造器/Registry 返回所属错误 | 只读 Clone；可跨执行共享底层 Arc |
| `ExecutionRecorder`、`ToolAuthorizer`、`AllowAllAuthorizer` | 普通启动路径直接需要的控制 SPI | begin/complete 错误阻断或终止；Deny 形成 ToolResult | 实现必须 `Send + Sync`，锁不得跨 I/O await |

SDK 不提供 prelude，也不重导 Provider、文件/Shell、Memory、Dispatcher、策略 Adapter 或
testkit 类型。

### 7.2 扩展 SPI：从能力所属 crate 导入

| 导入与类型家族 | 职责与所有权 | 失败语义 | 并发约束 |
| --- | --- | --- | --- |
| `agent_model::{ModelService, ModelRequest, ModelEvent, ModelError}` | 替换单次 Provider Turn | 建流前 `Err`；建流后唯一 `TurnFailed` | Service `Send + Sync`；不执行工具 |
| `agent_model::{RetryingModelService, ModelRetryPolicy, ModelAttemptObserver}` | 显式有限的建流前重试 | 只重试稳定瞬态错误；流建立后不介入 | 同一逻辑 Step、相同请求；等待响应取消 |
| `agent_tools::{Tool, ToolRegistry, ToolContext, ToolError}` | 自定义类型化工具并冻结 ToolSet | resolve/execute 错误成为绑定 call ID 的 ToolResult | Tool `Send + Sync`；取消后先清理资源 |
| `agent_tools::{FileSystemTool, ShellTool}` | 本地、远程或测试能力替换边界 | 稳定文件/Shell 错误分类 | 实现负责 I/O、输出限制、取消和进程清理 |
| `agent_core::{ExecutionRecorder, ToolAuthorizer}` | Journal 与最终授权闸替换边界 | Recorder 失败影响执行；Deny 不等于执行失败 | 每次 Run 绑定实例或上下文；实现 `Send + Sync` |
| `agent_core::{ToolPolicy, TypedToolPolicy, ComposedToolAuthorizer}` | 类型化策略装配 | 首个明确 Allow/Deny 生效，未匹配继续 | 评估只读 resolved facts，不执行副作用 |
| `agent_memory::{PinnedMemoryStore, RecallSource, MemoryRecall}` | Pinned Store、单 Source 与统一召回边界 | 稳定分类、部分 Source 失败可保留有效结果 | trait `Send + Sync`；持久化和网络由实现负责 |
| `agent_context::CompressionStrategy` | 生成压缩候选，不提交 Checkpoint | Candidate / NoOp / 稳定 CompactionError | 不持有 Session；上层控制交接次数与提交 |

### 7.3 高级 Adapter API：直接依赖底层 crate

| 导入与类型家族 | 职责与所有权 | 失败语义 | 并发约束 |
| --- | --- | --- | --- |
| `agent_core::{ExecutionSpec, AgentExecution::start, AgentEvent}` | 绕过 SDK 的低层 Core 装配与观察 | Core 受控终态和可丢普通事件 | 调用方自行冻结配置并组织控制面 |
| `agent_tools::{Dispatcher, ResolvedToolBatch, ResolvedToolInvocation}` | resolve、授权事实和一次性执行载荷 | invalid item 形成 ToolResult；重复执行是合约错误 | batch 只读借给 Authorizer，执行载荷只能消费一次 |
| `agent_tools::{SessionPathResolver, Fs*Tool, ShellExecTool, *Request/*Result}` | 标准工具壳、路径和能力 DTO | 构造、resolve 与能力错误保持分层 | 真实 I/O 由注入能力决定 |
| `agent_provider_openai_compatible::{Profile, OpenAiCompatibleService, Transport, ProviderWireObserver}` | OpenAI-compatible 路由、Codec、HTTP/SSE 与 wire 观察 | 构造、Transport、协议和 Provider 错误分层 | Service/Transport 可共享；wire observer 不得改变结果 |
| `agent_provider_openai_compatible::{Chat*, ChunkAssembler, SseParser, encode_request, decode_*}` | 自定义 Transport、fixture 和回放所需原生协议面 | 编解码错误不进入 Core 特判 | 单次 parser/assembler 由一个响应流拥有 |
| `agent_tools_local::{LocalFileSystem, LocalShell, EnvironmentPolicy}` | 当前用户权限下的真实本地 Adapter | OS/File/Shell 错误映射到能力错误 | 可共享；Shell 进程生命周期由 Adapter 负责 |
| `agent_context::{ContextLayout, RollingSummarySameModel, validate_replacement}` | Runtime 的布局、摘要候选和提交前校验 | 结构/策略/校验错误显式返回 | 不保存 Session 或 Checkpoint |
| `agent_memory::{PinnedMemorySnapshot, CoordinatedMemoryRecall, Recall* DTO}` | Prompt 渲染与多 Source 协调 | 限额、配置、取消和部分失败显式返回 | 协调器并发调用 Source，不替上层持久化 |
| `agent_types::*` | 规范消息、Part、ID、Tool Call/Result 和 Provider State | 受控构造与 Conversation 整体验证错误 | 无 I/O 的拥有值；可安全跨层传递 |

## 八、审查结论与兼容边界

- 没有把具体 Provider、Local Adapter、Memory 或 Demo 类型重导进 SDK。
- 没有发现只为单模块内部实现存在、同时又能安全收窄的根导出；因此首次基线没有公共可见性、
  名称或 serde 形状变化。
- 文件分页、精确替换和 UTF-8 截断辅助函数由 `agent-tools-local` 与 testkit 共同使用；
  Provider wire/Transport 类型由 Reliability Demo 回放使用；Context 布局与策略由 Runtime
  Harness 使用。这些接口继续保留在所属 crate。
- 新增公共 API 时应优先放在能力所属 crate。只有普通 Agent 构建或启动必须直接使用的类型，
  才考虑由 SDK 精选重导。
- 任何 Session、Run、Journal 实现、审批交互、HTTP DTO、UI 状态和 Demo 文件格式都不属于
  本候选公共面。

## 九、可运行参考

- [`minimal.rs`](../crates/agent-sdk/examples/minimal.rs)：空 ToolSet 与临时 Recorder。
- [`custom_controls.rs`](../crates/agent-sdk/examples/custom_controls.rs)：工具、显式 Recorder、
  Authorizer、事件与 Completion 并行消费。
- [`agent-sdk 模块约束`](modules/agent-sdk.md)：长期依赖与禁止边界。
- [`v0.7.0 技术方案`](versions/v0.7.0/技术方案.md)：本版本完整设计依据。
- [`v0.7.0 兼容性与性能基线`](versions/v0.7.0/兼容性与性能基线.md)：当前主机性能、能力矩阵和
  15 条验收证据。
