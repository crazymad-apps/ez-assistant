# Agent 系统技术架构

## 一、文档定位

本文定义 Agent 系统的上位技术架构，是 `agent-core`、`assistant-runtime`、`assistant-protocol` 和后续能力适配器的共同约束。各模块文档只能补充本模块细节，不能改变本文的职责和数据所有权边界。

修改 Agent 执行、模型协议、工具、记忆、安全、上下文、Run、会话持久化或相关跨层协议前，必须同时阅读本文和对应模块约束。

## 二、设计目标

- Agent 执行能力独立于 Tauri、UI、会话调度和具体部署形态。
- 同一执行引擎可用于纯本地客户端、C/S、B/S 和本地/远程混合架构。
- 模型、工具、记忆、安全、上下文和记录能力均通过稳定边界组合，不形成巨型 Agent 对象。
- Provider 特殊协议限制在适配层，不污染 Agent Loop。
- Runtime 持有业务 Run、会话和持久化权威状态；Core 只执行一次 `AgentExecution`。
- 所有策略来源在进入 Core 前解析为一份不可变执行规格，不在 Core 内重复维护 Profile、默认值和覆盖关系。

## 三、总体分层

```text
Desktop / Other Application
          │
          ▼
Assistant Runtime
├── Session / Message / Run
├── 调度、并发、恢复与配置编译
├── Conversation Journal
├── 权限记录与审计
└── 本地或远程能力适配器装配
          │ ExecutionSpec + ExecutionInput + bound services
          ▼
Agent Engine
├── Context Assembly
├── Single Model Turn
├── Tool Call Loop
├── Safety Decision
└── AgentExecution / Execution Events
          │
          ├── Model Service / Provider Codec
          ├── Tool Service
          ├── Memory Service
          ├── Safety / Approval Service
          └── Execution Recorder
```

依赖只能由上向下；Agent Engine 及其能力接口不得反向依赖 Runtime 或应用层。

## 四、核心术语和所有权

### 4.1 Runtime Run

`Run` 是 Runtime 的业务实体，拥有：

- `RunId`、`SessionId` 和持久化状态。
- 排队、串行、并发和取消句柄。
- 开始/结束时间、恢复、重试和审计记录。
- Core 事件与具体 Session/Run 的关联。

Agent Core 不创建、保存、查询或调度 `RunId`。

### 4.2 AgentExecution

`AgentExecution` 是 Core 中一次正在进行的执行实例。它由一次 `start` 调用和返回的独立句柄自然区分，不分配业务 ID，也不进入应用协议。

`AgentExecution` 持有本次执行的局部状态：

- 当前规范对话投影。
- 当前模型 step 和已完成工具调用。
- 本次执行预算和 Guardrail 观察状态。
- 事件流、完成结果和取消观察。

### 4.3 协议标识

Core 仍然必须持有模型协议正确性所需的标识，例如 `MessageId`、`PartId` 和 `ToolCallId`。这些标识用于消息排序及工具调用与结果配对，不代表业务 Run。

## 五、执行契约

### 5.1 不可变执行规格

Core 只接收一份已经合并、校验并解析完成的 `ExecutionSpec`：

```rust
pub struct ExecutionSpec {
    pub instructions: InstructionSet,
    pub model: ResolvedModel,
    pub tools: ToolSetSnapshot,
    pub context: ContextPolicy,
    pub memory: MemoryBinding,
    pub safety: SafetyPlan,
    pub budget: ExecutionBudget,
}
```

Core 不定义 `AgentProfile`。配置文件、用户偏好、Agent 模板、会话覆盖和临时参数均由 Runtime 合并；`ExecutionSpec` 是执行时唯一事实源。

### 5.2 执行输入与控制

业务输入和执行控制必须分离：

```rust
pub struct ExecutionInput {
    pub conversation: ConversationSnapshot,
    pub user_input: UserInput,
    pub attachments: Vec<Attachment>,
}

pub struct ExecutionContext {
    pub cancellation: CancellationToken,
    pub memory_scope: MemoryScope,
    pub recorder: Arc<dyn ExecutionRecorder>,
}
```

- `ExecutionInput` 只包含会影响模型语义的输入。
- `ExecutionContext` 只包含控制面和已经绑定当前 Runtime Run 的能力。
- Core 不通过 `ConversationRef` 自行加载 Session；Runtime 传入规范快照。
- `MemoryScope` 是不透明的能力作用域，不等同于 `SessionId`。

### 5.3 返回句柄

```rust
pub struct AgentExecution {
    pub events: AgentEventStream,
    pub completion: CompletionFuture,
    pub control: ExecutionControl,
}
```

Runtime 为每个 `AgentExecution` 单独消费事件，再附加自己的 `RunId` 和 `SessionId`。Core 事件不得为了方便 Runtime 合流而携带业务 ID。

## 六、执行状态机

```text
Preparing
    │
    ▼
BuildingContext
    │
    ▼
StreamingModel
    │
    ├── 无工具调用 ─────────────────────► Completed
    │
    └── Tool Calls
            │
            ▼
      RecordingAssistantMessage
            │
            ▼
        Authorizing
        │         │
      Allow      Deny：产生错误 ToolResult，对应工具不执行
        │         │
        ▼         │
       ExecutingTools
            │     │
            ▼     ▼
      RecordingToolResults
            │
            └────────────────────────────► BuildingContext
```

模型服务一次只执行一个 Provider Turn。工具调用后的继续、预算、取消和终止全部由 Agent Engine 显式控制，Provider 不得隐藏 Agent Loop。`Ask` 不在 Core 状态机内：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成。

## 七、规范对话与 Provider 隔离

### 7.1 规范对话

内部对话不得直接采用某一家 Provider 的 `role + content` 数据结构：

```rust
pub enum ConversationMessage {
    User(UserMessage),
    Assistant(AssistantMessage),
    Tool(ToolMessage),
    SystemUpdate(SystemMessage),
    ContextSummary(ContextSummary),
}

pub enum AssistantPart {
    Reasoning(ReasoningPart),
    Text(TextPart),
    ToolCall(ToolCall),
    ProviderState(OpaqueProviderState),
}
```

一次 Assistant 响应必须作为完整 Turn 保存，不能把 reasoning、文本和多个工具调用拆成无法恢复原始顺序的独立 UI 事件。

### 7.2 Provider Codec

Provider Adapter 由四个边界组成：

```text
Model Route → Protocol Codec → Transport → Stream Decoder
```

- Route：模型、端点、认证和能力声明。
- Codec：规范消息与 Provider 原生请求之间的转换。
- Transport：HTTP/WebSocket 和流帧传输。
- Decoder：Provider 流转换为规范 `ModelEvent`。

Provider Codec 必须保存继续下一轮所需的 reasoning、tool call、call ID 和不透明 Provider 状态。例如 [DeepSeek Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode/) 的工具调用轮次必须完整回传 `reasoning_content`；该约束只能存在于 DeepSeek/OpenAI-compatible Codec 和相应契约测试中，不能进入 Agent Loop 的条件分支。

## 八、权威记录与观察事件

### 8.1 Conversation Journal

Conversation Journal 由 Runtime 持有，是 Session 对话和恢复的权威状态。Core 不依赖 Repository 或数据库，而是通过已绑定当前 Runtime Run 的 `ExecutionRecorder` 提交两阶段 tool exchange：

```rust
#[async_trait]
pub trait ExecutionRecorder: Send + Sync {
    async fn begin_tool_exchange(
        &self,
        assistant: AssistantMessage,
    ) -> Result<ExchangeReceipt, RecordError>;

    async fn complete_tool_exchange(
        &self,
        receipt: &ExchangeReceipt,
        results: Vec<ToolMessage>,
    ) -> Result<(), RecordError>;
}
```

Runtime 的实现可以在内部持有 `RunId`、`SessionId`、事务和存储连接；这些信息不进入 Core 接口。
begin 产生的 pending exchange 是可恢复写前事实，不直接进入规范快照；complete 必须原子写入整批 ToolResult 并转为 completed。Runtime 恢复 pending 时先补齐 interrupted/unknown 结果，任何 `ConversationSnapshot` 都不得暴露未配对调用。

发生工具副作用前必须完成：

```text
完整 AssistantMessage
→ begin pending exchange 确认
→ 权限决策
→ 工具执行
→ 完整批次 ToolResult
→ complete exchange 原子确认
```

纯内存或测试场景使用内存 Recorder；是否持久化是装配选择，不改变执行语义和消息顺序。

### 8.2 Agent Event

`AgentEvent` 用于流式 UI、日志、指标和诊断，不是规范对话的事实源：

```text
ExecutionStarted
StepStarted
TextDelta
ToolProposed
ToolStarted
ToolOutput
ToolCompleted
GuardrailTriggered
ContextCompacted
ExecutionCompleted / Failed / Cancelled
```

审批事件由 Runtime 自行产生，不属于 `AgentEvent`。

- Runtime 可以给事件附加 `RunId`、`SessionId` 和序号后持久化或广播。
- UI 断线后以 Runtime 快照恢复，不能依赖重放所有 token delta。
- UI 或观察者事件订阅断开不能改变 Provider 对话内容；Recorder 失败则必须阻止后续副作用并终止或受控恢复。

## 九、记忆扩展架构

### 9.1 稳定窄接口

Agent Engine 只依赖：

```rust
#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn recall(
        &self,
        request: RecallRequest,
    ) -> Result<MemoryContext, MemoryError>;

    async fn record(
        &self,
        observation: MemoryObservation,
    ) -> Result<(), MemoryError>;
}
```

### 9.2 插件协议

记忆插件抽象完整记忆能力，而不只抽象数据库 CRUD：

```rust
#[async_trait]
pub trait MemoryPlugin: Send + Sync {
    fn descriptor(&self) -> MemoryPluginDescriptor;
    async fn recall(&self, request: RecallRequest) -> Result<RecallResponse, MemoryError>;
    async fn observe(&self, observation: MemoryObservation) -> Result<(), MemoryError>;
    async fn consolidate(&self, request: ConsolidationRequest)
        -> Result<ConsolidationResult, MemoryError>;
    async fn mutate(&self, command: MemoryCommand)
        -> Result<MemoryMutationResult, MemoryError>;
}
```

`MemoryOrchestrator` 在 Agent Engine 外部组合插件，并负责并行召回、超时隔离、去重、评分归一、token 配额、写入路由和降级。支持但不限于：

- 本地嵌入式记忆。
- 远程 C/S 或 B/S 记忆服务。
- 本地短期加云端长期的混合记忆。
- 多来源组合记忆。
- 完全关闭记忆的 No-op 实现。

记忆插件需要暴露模型工具时，必须作为普通 Tool Contribution 注册，禁止 Memory SPI 直接依赖 Tool SPI。

## 十、安全、预算与 Guardrail

三类约束必须分开：

### 10.1 协议正确性

以下是不允许关闭的正确性要求，不称为 Guardrail：

- Tool 输入和输出 schema 校验。
- Tool call 与 result 的 ID 配对及顺序。
- 模型流唯一终态。
- 取消和失败传播。
- 未完成调用不得记录成成功。

### 10.2 资源预算

```rust
pub struct ExecutionBudget {
    pub max_steps: Option<u32>,
    pub max_tool_calls: Option<u32>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_duration: Option<Duration>,
    pub max_tool_output_bytes: Option<u64>,
}
```

`None` 表示不设该项上限。Core 不注入隐藏默认值；默认值属于 Runtime 配置编译。

### 10.3 启发式 Guardrail

完全相同调用、连续失败和无进展检测分别支持：

```rust
pub enum EnforcementMode {
    Off,
    Observe,
    Enforce,
}
```

- `Off`：不检测。
- `Observe`：只发诊断事件。
- `Enforce`：按显式阈值警告、阻断或终止。

每条规则独立配置；Core 不允许未声明的启发式规则静默中止执行。

### 10.4 权限与审批

Core 授权决策仅 `Allow` / `Deny`：`Deny` 在授权闸处转换为错误 ToolResult（回喂模型、驱动循环继续的唯一载体是 error ToolResult），对应工具不执行，循环继续。Core 只保证逐 Tool Call 独立过闸；审批编排（串行询问、攒批询问、规则自动放行）归 Runtime 的 authorizer 实现，Core 在 authorize 时提供本轮批次上下文（同轮全部 tool call）供其实现批次审批交互。`Ask` 不进 Core 词汇表：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成；`ApprovalService` 归 Runtime/Adapter。规则保存、UI 交互、无人值守策略和审计属于 Runtime/Adapter；模型输出不得绕过安全决策直接触发工具副作用。

## 十一、工具边界

- Tool 使用结构化定义、输入 schema、输出 schema 和稳定错误。
- Tool Registry 在 `ExecutionSpec` 编译阶段生成不可变快照；执行期间不修改模型可见工具集合。
- Dispatcher 一次只负责一个规范 Tool Call，不负责模型请求和 Agent 继续循环。
- 同批工具是否并行由显式执行策略决定；有副作用的工具默认不推断为可安全并行。
- 文件 read/list/search/write/delete/edit 共享文件能力实现；`grep`/`rg` 是 `search` 的内部后端。
- Shell 是独立通用工具，其当前用户权限能力不能被描述为文件沙盒。
- 真实文件、Shell、网络和桌面工具实现位于 Runtime/Adapter，不进入 Agent Engine。

## 十二、Context 与 Memory 的边界

Context Engine 只处理当前模型上下文窗口：

- 稳定 system 前缀和动态上下文组装。
- token 估算与预算。
- 工具输出裁剪。
- 压缩、摘要和 Provider overflow 恢复。

Memory Service 处理跨执行的召回和观察。Memory 返回的内容只是 Context 的一个输入来源；Context 不写长期记忆，Memory 不直接修改规范会话。

上下文压缩不得破坏仍需回传的 Provider 状态、未完成工具调用或 Tool Call/Result 配对。Provider overflow 恢复必须有显式次数上限，不能形成隐藏重试循环。

## 十三、物理组织原则

目标逻辑组件包括：

```text
agent-types
agent-model
agent-tools
agent-memory
agent-safety
agent-context
agent-recording
agent-engine
agent-sdk
agent-testkit
```

逻辑边界必须从第一天成立；是否立即拆成独立 crate 由开发计划决定。满足以下条件之一时应拆 crate：

- 需要被 Agent Engine 之外的稳定调用方独立使用。
- 存在多个独立 Adapter，需要阻止反向依赖。
- 编译期边界能显著减少 Provider、存储或应用依赖泄漏。

不能仅为目录整齐创建没有替换边界和独立调用方的空 crate。

## 十四、Harness 验证

Agent Core 必须可以在不启动 Tauri、数据库和真实网络的情况下验证：

- 纯文本完成和流式事件顺序。
- 单个和多个工具调用。
- AssistantMessage 在工具副作用前完成记录。
- DeepSeek reasoning + tool call + result 的往返保真。
- 模型、工具、Recorder、Memory 和 Approval 失败。
- 取消传播。
- 有限和无限预算语义。
- Guardrail 的 `Off`、`Observe`、`Enforce`。
- Memory Plugin 超时、部分失败、聚合和 No-op。
- UI 或观察者事件订阅断开不破坏规范对话。

测试优先使用 scripted model、fake tool、in-memory recorder、fake memory、fake approval 和确定性时钟，不依赖真实 Provider 的偶然输出。
