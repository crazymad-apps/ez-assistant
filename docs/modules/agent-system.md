# Agent 系统技术架构

## 一、文档定位

本文定义 Agent 系统的上位技术架构，是 `agent-core`、`assistant-runtime`、`assistant-protocol` 和后续能力适配器的共同约束。各模块文档只能补充本模块细节，不能改变本文的职责和数据所有权边界。

修改 Agent 执行、模型协议、工具、记忆、安全、上下文、Run、会话持久化或相关跨层协议前，必须同时阅读本文和对应模块约束。

## 二、设计目标

- Agent 执行能力独立于 Tauri、UI、会话调度和具体部署形态。
- 同一执行引擎可用于纯本地客户端、C/S、B/S 和本地/远程混合架构。
- 模型、工具、安全、上下文和记录能力均通过稳定边界组合；记忆通过冻结 Prompt 与普通
  工具接入，不形成巨型 Agent 对象或 Core 专用阶段。
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
├── Session Prompt Snapshot / Pinned Memory Store
├── RecallSource 与记忆工具能力装配
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
          ├── Safety / Approval Service
          └── Execution Recorder
```

依赖只能由上向下；Agent Engine 及其能力接口不得反向依赖 Runtime 或应用层。

正式产品的物理运行形态为：

```text
Tauri Desktop 进程
        │ assistant-protocol / local Runtime Client
        ▼
Runtime Host 进程
        └── assistant-runtime → agent-sdk / agent-core
```

Runtime Host 是正式产品进程；Session 是该进程内的逻辑单元，不创建会话级操作系统子进程。
系统 daemon、开机启动和崩溃拉起是后续平台生命周期能力，不能与独立 Runtime 进程混为一谈。

窗口判断、规范历史布局、replacement 校验和压缩策略边界位于共享
`agent-context` crate。v0.3.0 的正式调用方是 `agent-core`，临时
`runtime-harness` 直接装配其余能力完成版本验证；正式 `assistant-runtime` 是否以及
如何接入留待总体设计。`agent-core`、Harness 和未来 Runtime 都只能向下依赖它；
`agent-context` 只依赖 `agent-model` 和 `agent-types`，不得反向依赖 Core、Runtime、
Provider Adapter、应用协议或存储。

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
    pub system_prompt: SystemPromptSnapshot,
    pub model: Arc<dyn ModelService>,
    pub tools: ToolSetSnapshot,
    pub context_window: Arc<ContextWindowEvaluator>,
    pub model_request: ModelRequestConfig,
    pub budget: ExecutionBudget,
    pub guardrails: Option<GuardrailConfig>,
}
```

Core 不定义 `AgentProfile`。配置文件、用户偏好、Agent 模板、会话覆盖和临时参数均由 Runtime 合并；`ExecutionSpec` 是执行时唯一事实源。

`ModelRequestConfig` 只冻结 `tool_choice`、generation、reasoning 和命名空间化
Provider Options，并在同一 AgentExecution 的每个 Model Step 原样复用。endpoint、credential、
model 和 context window 仍属于 ModelService 构造；Core 不按 Provider 名称分支或自动启用
DeepSeek thinking。

### 5.2 执行输入与控制

业务输入和执行控制必须分离：

```rust
pub struct ExecutionInput {
    pub conversation: ConversationSnapshot,
}

pub struct ExecutionContext {
    pub cancellation: CancellationToken,
    pub recorder: Arc<dyn ExecutionRecorder>,
    pub authorizer: Arc<dyn ToolAuthorizer>,
}
```

- `ExecutionInput` 只包含 Runtime 已经选定的完整有效对话快照；新用户 Run 必须先将
  UserMessage 写入规范历史再构造 Snapshot，Core 不再单独追加用户输入。
- `ExecutionContext` 只包含取消、落账和授权控制面。
- Core 不通过 `ConversationRef` 自行加载 Session；Runtime 传入规范快照。

### 5.3 返回句柄

```rust
pub struct AgentExecution {
    pub events: AgentEventStream,
    pub completion: CompletionFuture,
    pub control: ExecutionControl,
}
```

Runtime 为每个 `AgentExecution` 单独消费事件，再附加自己的 `RunId` 和 `SessionId`。Core 事件不得为了方便 Runtime 合流而携带业务 ID。

Runtime 必须显式持有消费事件和等待完成结果的 Run supervisor。受控关闭先传播取消并在
有限时间内等待 supervisor；超过上限后可终止 supervisor，并由 Runtime 将仍处于活动态的业务
Run 强制结算为内部失败。该兜底只保证 Runtime Host 不会无限等待，不代表 Core 已完整观察其
内部执行 task 的 panic/JoinError；Core 自身的完成句柄契约需要单独演进。

## 六、执行状态机

```text
Preparing
    │
    ▼
BuildingContext
    │
    ├── 达到压缩阈值 ────────────────► CompactionRequired
    │
    ▼
StreamingModel
    │
    ├── Context Overflow ─────────────► CompactionRequired
    │
    ├── 无工具调用 ─────────────────────► Completed
    │
    └── Tool Calls
            │
            ▼
      RecordingAssistantMessage
            │
            ▼
      ResolvingToolInvocations
        │
        ├── 未知工具/参数无效：产生错误 ToolResult
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

模型服务一次只执行一个 Provider Turn。工具调用后的继续、预算、取消和终止全部由 Agent Engine 显式控制，Provider 不得隐藏 Agent Loop。上下文压缩是例外的 Runtime 后继动作：Core 以 CompactionRequired 终态结束当前执行，Runtime 压缩后启动新的 continuation，而不是 Core 在原执行内重试。`Ask` 不在 Core 状态机内：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成。

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

### 7.3 模型建立重试

Provider-neutral 的模型重试只允许包装 `ModelService::stream()` 的建流前错误。上层必须显式
装配有限策略；错误提供 Connection、Timeout、RateLimited、Unavailable 与 Retry-After 等稳定
事实，但不替上层决定是否重试。

- 每个 attempt 使用完全相同的 `ModelRequest`，属于同一个 Core Model Step。
- attempt 数、等待和取消必须可观察；等待与下一次请求都必须响应取消。
- `stream()` 一旦返回事件流，任何后续 `TurnFailed` 都结束当前 Step，不透明重试或拼接输出。
- Config、Auth、Protocol、ToolArguments、ContextOverflow 和 Cancelled 不进入通用重试。
- Tool、Recorder、Pinned Store 和 RecallSource 不因模型重试能力获得通用自动重试。

Core 不知道 attempt，也不增加重试状态机；Runtime 或其他上层宿主只通过装配后的单个
`ModelService` 使用该能力。

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
UsageUpdated
TextDelta
ToolProposed
ToolStarted
ToolOutput
ToolCompleted
GuardrailTriggered
ExecutionCompleted / Failed / Cancelled / CompactionRequired
```

审批事件由 Runtime 自行产生，不属于 `AgentEvent`。
compression operation、Context Checkpoint 提交与 continuation 由 Runtime 事件表达，
不伪装成 Core `AgentEvent`。

- Runtime 可以给事件附加 `RunId`、`SessionId` 和序号后持久化或广播。
- UI 断线后以 Runtime 快照恢复，不能依赖重放所有 token delta。
- UI 或观察者事件订阅断开不能改变 Provider 对话内容；Recorder 失败则必须阻止后续副作用并终止或受控恢复。

### 8.3 完整 Trace 与分层回放

完整 Trace 是 Runtime 所有的高敏诊断副本，不是 Conversation Journal。记录点直接复用各层
已经拥有的事实：完整 `ModelRequest` / `ModelEvent`、Provider 编码请求与 raw response、
`AgentEvent` 以及 Runtime/Journal 事件；只为当前缺失的 Provider wire 和 retry attempt 增加
所属层事件，不建立平行 Trace 业务模型。

- Runtime 为 Session、Run、AgentExecution、逻辑模型调用和 attempt 建立关联，并选择关闭、
  元数据或 Full Replay 录制以及持久化、权限和保留策略。
- Core、Model、Provider 和 Tool Adapter 只提供观察接缝，不直接写文件或数据库。
- Authorization、API key、Cookie、代理认证和其他 credential 在记录类型构造前永久排除；
  Full Trace 的其余 prompt、reasoning、工具参数与正文仍按高敏数据管理。
- Trace 写入、队列或容量失败只把记录标为 Incomplete，不改变模型或 AgentExecution 结果；
  Journal 提交失败仍按权威业务错误处理。
- Wire Replay 重走真实 Provider Decoder，Model Replay 重现规范模型服务，Timeline 只读展示；
  三者都不重新执行历史真实工具、审批或 Journal 副作用。
- debug-viewer 继续是允许丢弃的开发观察通道，不能作为 Full Replay 的权威来源。

v0.6.0 不修改正式 Runtime，由顶层 `tools/reliability-demo` 私有验证 Collector、JSONL、完整性
和回放；其文件、事件封装和 CLI 不是产品契约。

## 九、记忆扩展架构

### 9.1 Pinned Memory

少量常驻记忆由实现无关的 `PinnedMemoryStore` 管理。创建新 Session 时，上层读取当前
条目并确定性渲染为一个 System Prompt Part，随后与其他 system parts 一起冻结为
`SystemPromptSnapshot`。修改 Store 不刷新当前 Session；恢复、继续、压缩续接和分支
必须复用原快照，不能读取最新 Store 重建。

最终 Session 只需保存渲染完成的 System Prompt 状态。结构化条目、容量规则和 Store Adapter
属于初始化输入与能力装配，不进入 Core 执行状态机。

### 9.2 Memory Recall

大量或外置信息只通过普通 `recall_memory` 工具按需进入当前工具上下文。`RecallSource`
表示可替换的数据源适配器，具体 `MemoryRecall` 实现可以协调多个 Source，负责显式默认
集合、并发、超时、确定排序、精确去重、截断和部分失败。

- Recall 结果天然携带来源，不自动写入 Pinned Memory 或修改 Session Prompt。
- Source 是否对模型可见、是否允许模型指定，由 System Prompt、Skill 和上层授权决定，
  不进入 Source trait 或动态工具定义。
- Core 只看到标准 Tool Call/Result，不依赖 Store、MemoryRecall 或 RecallSource。
- `agent-memory` 只定义实现无关的领域、能力契约、快照渲染和协调逻辑；本地、远程和
  混合 Adapter 由 Runtime、应用或验证宿主实现。
- 本版本不引入 MemoryService、MemoryPlugin、公共 Orchestrator、自动观察、后台整合、
  自动淘汰或自动写入链路。

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

启用的完全相同调用、连续失败和无进展检测分别支持：

```rust
pub enum ActiveGuardrailMode {
    Observe,
    Enforce,
}
```

- 未配置对应检测器：不检测。
- `Observe`：只发诊断事件。
- `Enforce`：按显式阈值警告、阻断或终止。

整体 Guardrail 配置和每个检测器配置均为可选；Core 不允许未声明的启发式规则静默
中止执行。

v0.4.0 首期只实现完全相同调用和连续失败；无进展检测在没有可靠通用信号前保留为
后续能力。关闭通过未配置对应检测器表达，Observe 只发诊断事件，Enforce 才在完整
结算 Tool Call/Result 后终止当前 AgentExecution。

### 10.4 权限与审批

Core 授权决策仅 `Allow` / `Deny`：`Deny` 在授权闸处转换为错误 ToolResult（回喂模型、驱动循环继续的唯一载体是 error ToolResult），对应工具不执行，循环继续。工具名查找、类型化参数校验、模型可见默认值落实和确定性路径解析必须先形成 resolved invocation；Authorizer、Guardrail 和执行器使用同一份冻结事实，未知工具和无效输入不进入授权。Core 只保证逐 Tool Call 独立过闸，并提供按顺序组合类型化策略与最终 Authorizer 的通用机制；审批编排（串行询问、攒批询问、规则自动放行）归 Runtime authorizer 实现，authorize 时可观察本轮 resolved batch。`Ask` 不进 Core 词汇表：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成；`ApprovalService` 归 Runtime/Adapter。Plan/Build 等能力上限必须先于 Ask/Auto fallback，Auto 不能覆盖上层明确 Deny。规则保存、UI 交互、无人值守策略和审计属于 Runtime/Adapter；模型输出不得绕过安全决策直接触发工具副作用。

## 十一、工具边界

- Tool 使用结构化定义、输入 schema、输出 schema 和稳定错误。
- Dispatcher 分为无副作用 resolve 与一次性 execute；类型化输入只解析一次，授权和
  执行使用同一 resolved invocation。
- 会影响返回范围、时长或副作用的工具默认值必须进入冻结 Tool Definition，不能只
  隐藏在执行器构造参数中。
- Tool Registry 在 `ExecutionSpec` 编译阶段生成不可变快照；执行期间不修改模型可见工具集合。
- Dispatcher 对一个规范 Tool Call 批次做无副作用 resolve，并按明确位置一次性
  execute；它不负责策略、模型请求或 Agent 继续循环。
- 同批工具是否并行由显式执行策略决定；有副作用的工具默认不推断为可安全并行。
- 文件 read/list/search/write/delete/edit 共享文件能力实现；`grep`/`rg` 是 `search` 的内部后端。
- Shell 是独立通用工具，其当前用户权限能力不能被描述为文件沙盒。
- 真实文件与 Shell 机制位于独立本地基础设施 Adapter；Runtime 负责装配工作目录、
  能力策略、审批、环境过滤和审计。真实网络和桌面工具同样位于 Runtime/Adapter，
  均不进入 Agent Engine。

## 十二、Context 与 Memory 的边界

Context Engine 只处理当前模型上下文窗口：

- 稳定 system 前缀和动态上下文组装。
- 基于 Provider usage 与模型窗口的统一上下文占用判断；需要时可由后续版本扩展估算器。
- 工具输出裁剪。
- 压缩、摘要和 Provider overflow 恢复。

Pinned Memory 在 Session 创建前渲染并成为冻结 system prefix；Memory Recall 的结果只以
普通 Tool Result 进入规范对话。Context 不读写 Store 或 Source，不自动把当前对话转成
长期记忆；记忆能力也不直接修改规范会话。

已确定的正式层级边界是：模型服务显式提供 `context_window_tokens`；共享 Context
Window Evaluator 只使用最近完整 Provider Result 的 `total_tokens` 计算窗口比例；
Core 在每个 Model Step 前执行该判断；Provider/Model 层只报告 provider-neutral
Context Overflow；Core 将 Run 内预算阈值或 Overflow 转换为 CompactionRequired
可靠终态。Core 和 Provider 均不得在原执行内隐藏压缩或流建立后的重试；显式装配的模型
建立重试仍属于同一个 Model Step，并遵守 §7.3 的严格边界。

Run 前判断、Checkpoint、用户主动压缩、恢复次数和 continuation 是未来 Runtime
需要承接的行为目标，但其 Session、Run、Repository、事件和并发接口尚未完成总体
设计。v0.3.0 仅由 `runtime-harness` 使用私有临时类型验证以下语义：用户消息先落账；
Run 前压缩成功后再创建首个 Run；Core 交接后压缩成功才 continuation；用户主动压缩
不启动 Core、不生成 UserMessage、也不续跑。

Harness 中 Run 前阈值、Run 内阈值、Provider Overflow 和用户主动压缩四条路径复用
同一个私有压缩编排入口；该入口调用 `agent-context` 的 Layout、Strategy 和 Validator
后提交内存 Checkpoint。该私有入口是行为验证代码，不是正式 Runtime 公共契约。

上下文压缩不得破坏仍需回传的 Provider 状态、未完成工具调用或 Tool Call/Result
配对。Overflow 所在的未完成 Step 整体丢弃；Core 只有在完整 TurnFinished 后才允许
记录和执行 Tool Call，因此交接边界取上一个已完整提交的 Step。Provider overflow
恢复必须有上层编排管理的显式次数上限，不能形成隐藏重试循环。

## 十三、物理组织原则

目标逻辑组件包括：

```text
agent-types
agent-model
agent-tools
agent-tools-local
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

v0.3.0 将 Context Engine 拆为 `agent-context`：Core 的每个 Model Step Preflight
是正式调用方，Harness 的 Run 前判断与压缩编排是临时验证调用方。共享
Evaluator/Layout/Validator/Strategy 可以阻止规则分叉；Harness 私有的 Session、Run、
Checkpoint、恢复次数和 continuation 不进入产品 crate。`ModelRequest` 已是统一请求
契约，Core 和压缩策略按各自明确语义直接构造，不增加一一映射的 Compiler/Input DTO。

v0.4.0 将真实文件与 Shell 机制拆为 `agent-tools-local`：该 crate 只向下依赖
`agent-tools`，不感知 Core、工作模式、审批、审计或 UI。`tools/safety-demo` 是验证
调用方和顶层独立开发工具，可装配 Agent Core、Provider 与本地 Adapter，并只在
loopback 提供临时安全页面；其 Session、Run、审批、审计和 HTTP DTO 均保持私有，
不得上提为正式 Runtime 或应用协议。

v0.5.0 将实现无关的 Pinned Memory 与 RecallSource 契约拆为 `agent-memory`；标准
记忆工具壳由 `agent-tools` 提供。`tools/memory-demo` 是顶层验证调用方，可私有实现
JSON Store、RecallSource、Session 和 Journal，但这些类型与文件格式不得上提为正式
Runtime 契约。仓库不建立 `agent-memory-local`，未来 Adapter 由实际应用装配选择。

v0.6.0 不新建 recording 或 replay 公共 crate：Provider wire 事件归具体 Adapter，模型 attempt
事件与建立重试归 `agent-model`；正式持久化编排未来归 Runtime。`tools/reliability-demo` 是顶层
验证调用方，私有实现 Trace 文件、Loader、ReplayTransport、ReplayModelService、Timeline 和
宿主事件，不得把这些临时类型上提为 Runtime 或应用协议。

v0.7.0 新增 `agent-sdk` 作为 `agent-core` 的薄 Facade：一个 Agent 对应一份冻结的
Model、System Prompt、Context Window、ToolSet、请求配置、Budget 和 Guardrail 装配，最终仍
调用唯一的 `AgentExecution::start`。SDK 不持有动态 Conversation、Session、Run、Journal、
审批、Store 或调度状态，也不依赖具体 Provider、本地 Adapter 或应用层。同一会话的执行串行由
上层保证，不同 Agent 可以共享线程安全的底层服务并由 Tokio 异步并发。
普通入口、扩展 SPI 与高级 Adapter API 的候选分层记录在
[`Agent SDK 导读`](../Agent-SDK导读.md)；SDK 只精选重导普通启动路径直接
需要的类型，不隐藏底层扩展边界。

`tools/core-demo` 是 v0.7.0 的顶层 B/S 验证宿主，可以私有实现多 Session、内存 Journal、
安全/审批、JSON Memory Store、Context continuation 和 HTTP/SSE 页面，并装配真实 Provider 与
本地 Adapter。它不属于正式 Runtime、Protocol、Desktop、sidecar 或 daemon；这些私有类型、
文件格式和 HTTP 契约均不得上提为产品或 SDK API。

v0.8.0 开始建立正式 `assistant-runtime` 与 `apps/runtime-host`：Runtime library 持有内存 Session、
Conversation、Run 和执行监督的权威状态，Host 只负责进程入口、具体资源装配与私有本地通信。
本版本的 Host 是手动启动的正式产品进程，不是 `tools/*` Demo，也不等同于已经实现系统 daemon；
Unix Socket wire 和命令行验证客户端不构成公共应用协议。

v0.9.0 由 Runtime 接管 Runtime Home 配置、脱敏诊断、model key 和 reload：Session 只冻结
model key 与已渲染 System Prompt，每个 Run 按开始时取得的同一配置快照构造完整 Agent 和
ModelService。Host 不保存第二份配置状态；reload 只影响后续 Run。Runtime 同时采用有界关闭并
持有 Run supervisor 的终止能力，Host 显式观察连接子任务故障；Core 内部执行 task 的完整
JoinError 所有权留到 v0.10.0。

## 十四、Harness 验证

Agent Core 必须可以在不启动 Tauri、数据库和真实网络的情况下验证：

- 纯文本完成和流式事件顺序。
- 单个和多个工具调用。
- AssistantMessage 在工具副作用前完成记录。
- DeepSeek reasoning + tool call + result 的往返保真。
- 模型、工具、Recorder 和 Authorizer 失败。
- 取消传播。
- 有限和无限预算语义。
- Guardrail 的 `Off`、`Observe`、`Enforce`。
- 普通记忆工具的 Source 超时、部分失败、来源保留和多轮继续。
- UI 或观察者事件订阅断开不破坏规范对话。
- 建流前有限重试、Retry-After、取消和 attempt 关联。
- Provider wire 观察前后请求、分块、错误和生命周期等价。
- Complete/Incomplete、损坏 Trace、Wire/Model Replay 和并发调用隔离。

测试优先使用 scripted model、fake tool、scripted RecallSource、in-memory recorder、
fake authorizer 和确定性时钟，不依赖真实 Provider 的偶然输出。
