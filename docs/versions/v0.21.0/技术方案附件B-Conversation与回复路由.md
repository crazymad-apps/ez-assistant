# v0.21.0 技术方案附件 B：Conversation、输入来源与渠道投递

## 一、文档定位

- 状态：已确认（2026-08-27）；M9 于 2026-08-31 按附件 E 收敛输入与续跑口径
- 上位方案：[`v0.21.0 技术方案`](技术方案.md)
- 约束范围：Channel 输入进入 Session、既有 Input/Run/Conversation 扩展、PC 输出托管、附加渠道投递、
  输出调度和恢复语义

本附件坚持一条核心原则：Agent 在 Runtime 中运行，Session/Conversation 是唯一规范业务事实。
Desktop UI、Device 和未来 IM 都是输入与展示渠道；其中 Desktop UI 必须始终能从规范
Conversation 看到所有显式 User Input 和 Assistant Output。任何托管或附加投递都不能隐藏、
移动或复制这些正文，也不创建 Device Conversation、Delivery Store 或第二份 Assistant Message。

## 二、三个正交事实

每一轮必须分开表达：

1. **规范 Conversation**：显式输入和最终 Assistant 正文始终只保存一份，Desktop UI 强制可见；
2. **输入来源**：谁从哪个 Channel 发起了这条 Input；
3. **附加投递集合**：规范正文结算后，还需要向哪些 Channel 投递，以及每个投递的文字/音频偏好。

不能用“当前连接”“当前选中设备”或模型 Tool 参数替代这三个事实。
附加投递是零到多个，不是 Desktop 与 Device/IM 之间的互斥路由。

| 触发/输入类型 | Desktop UI 规范内容 | 附加投递 | 投递形态来源 |
| --- | --- | --- | --- |
| Desktop 主控输入 | 始终保留输入和最终回复 | 有 PC 托管时投递该 Device；无托管时本版不产生附加投递 | 目标设备当前预设 |
| Device 主控输入 | 始终保留 transcript/文字和最终回复 | 投递回冻结的来源 Device | Input 冻结的请求偏好 |
| `ControllerDelivery` | 主控和普通会话各自保留规范事实 | 目标普通会话不直接向外部 Channel 投递 | 携带后续报告返回基准 |
| `ProxyReport` 携带来源 Device | 始终保留报告和最终回复 | 投递回该 Device | 原主控输入冻结的请求偏好 |
| `ProxyReport` 使用主控默认路由 | 始终保留报告和最终回复 | 有 PC 托管时投递该 Device；否则本版无附加投递 | 目标设备当前预设 |
| 同轮 Runtime continuation | 始终保留最终结算内容 | 不创建新决策，继承所属逻辑输出周期 | 继承所属周期 |
| 未来其他 Channel | 始终保留显式输入和最终回复 | 可返回来源 Channel，也可按明确产品规则同时投递其他 Channel | 各投递自己的合法偏好 |

设备来源优先级高于 PC 输出托管：一个终端主动发起的输入及其明确关联的代理报告都回复该终端。
代理报告没有明确外部来源时，不猜测最后活跃设备，而是回退到主控默认路由，因此仍需读取结算时的
PC 输出托管。

## 三、Runtime 输入入口

### 3.1 外部协议保持不变，Host 统一归一化

公共 `SubmitInputRequest` 继续只表达 Desktop 用户意图，不增加可由任意客户端伪造的 `source`、
`device_id` 或 reply target 字段。Desktop HTTP Command 仍使用该 wire DTO，Host 在调用 Runtime 前
固定补入 Desktop `Text + Text` 来源事实。

```rust
AssistantRuntime::submit_session_input(SubmitSessionInputRequest {
    input: desktop_request,
    source: InputChannelSource::Desktop(DesktopInputSource { /* ... */ }),
})
```

该转换不改变当前客户端协议。未来 Desktop 语音输入由 Host 的本地媒体 Adapter 在 ASR 完成后构造
`Desktop { modality: SpeechTranscript, requested_output: ... }`；不让客户端伪造其他 Channel 身份。

### 3.2 Host 私有的统一 Session 输入入口

`assistant-runtime` 对 Host 提供以 Session 为目标实体的受信任 Channel Input 用例。它不是 Device
WSS 或 HTTP 网络路由；首个调用方是 Device Gateway 上层的 Channel Router，未来其他受信任
Channel 可以复用相同 Session 入口并扩展既有 `InputChannelSource`：

```rust
pub struct SubmitSessionInputRequest {
    pub input: SubmitInputRequest,
    pub source: InputChannelSource,
}

pub async fn submit_session_input(
    &self,
    request: SubmitSessionInputRequest,
) -> RuntimeResult<SubmitInputResult>;
```

该 DTO 属于 `assistant-runtime` 的 Host 端口，不进入 `assistant-protocol::RuntimeCommand`，也不派生
序列化。`input` 复用既有产品输入意图，`source` 只补充可信 Channel 来源；Runtime 以目标 Session、
source 的稳定身份与 client input 身份完成一致性核对，Device 来源由 Runtime 派生幂等 key。

Desktop 的目标来自已认证 Desktop Command；本版本 Device Channel Router 每次读取 Runtime 权威的
活动 Session 列表并选择当前产品 Controller，不缓存 Session ID、角色或最后活跃会话。该选择属于
Host 产品路由策略，不写入 Runtime 通用输入准入。

Device Gateway 只能用已认证 connection owner 填写 Device ID；WSS payload 中的自报值只用于核对，
不能直接成为请求身份或目标 Session。Desktop 与 Device Adapter 都调用同一个 Runtime 用例。

入口校验：

- 目标 Session 必须存在、active、未故障且可接受 Input；
- 设备必须仍为 paired；
- Host 必须已完成连接认证与 capability 协商；Runtime 只校验持久设备状态、结构化 modality 和
  output preference 的业务合法性，不读取连接 capability 快照；
- message 非空且满足现有文本上限；
- Runtime 派生的 idempotency key 与 Channel kind、稳定 source identity 和 client input identity 一致；
- Device 来源不允许附件、Skill 选择或 Goal mode；其目标选择只由 Host Router 决定；
- Runtime 不要求 Device 来源的目标角色必须为 Controller，因此未来 Channel 不需要复制输入用例。

通过后，Runtime 只沿一套 Input 接受、Queue 和首次 Run 创建逻辑落账。`InputOrigin` 仍为 `User`，
UserMessage 仍为 `UserMessageOrigin::User + TranscriptVisibility::Visible`。

Device 来源在同一规范 UserMessage 中增加一个 `kind = channel_input` 的
`UserPart::InternalContext`，复用既有 canonical insertion 机制，不新增消息、存储或请求期旁路。该 Part
冻结并向模型说明 `source = intelligent_terminal`、`input_modality`、`reply_preference` 以及相应投递指令：
语音或文字加语音偏好要求本轮调用 `speak` 形成简短自然的播报文本，纯文字偏好则不为渠道投递调用
`speak`。该 Part 模型可见但产品转录隐藏；终端正文、来源投影与 Conversation 顺序均保持原有语义。
Desktop 公共输入入口不插入 `channel_input`，Desktop User Message 保持现状。

## 四、Input Channel Source

### 4.1 领域结构

在 `StoredInput`、`NewStoredInput` 和接受事务中增加：

```rust
pub struct StoredInput {
    // existing fields...
    pub channel_source: Option<InputChannelSource>,
}

pub enum InputChannelSource {
    Desktop(DesktopInputSource),
    Device(DeviceInputSource),
}

pub struct DesktopInputSource {
    pub modality: InputModality,
    pub requested_output: OutputPreference,
}

pub struct DeviceInputSource {
    pub device_id: DeviceId,
    pub client_input_id: String,
    pub modality: InputModality,
    pub requested_output: OutputPreference,
}

pub enum InputModality {
    Text,
    SpeechTranscript,
}

pub enum OutputPreference {
    Text,
    Audio,
    TextAndAudio,
}
```

`InputModality` 和 `OutputPreference` 是 Channel 通用业务语义，不属于 Device 协议。
`SpeechTranscript` 表示 UserMessage 正文是 Host ASR 的最终 transcript，不保存或引用音频路径。
开发临时音频文件不进入该结构。当前 Desktop 写入 `Text + Text`；未来开启语音交互时
仍复用这两个枚举，不增加 Desktop 专用模态类型。

### 4.2 与现有来源的关系

- `InputOrigin` 回答“用户还是 Runtime 创建”，保持 `User | Runtime`；
- `CrossSessionInputBinding` 回答“当前跨会话消息是主控投递还是代理报告”；
- `InputChannelSource` 回答“真实用户从哪个产品 Channel 进入”，只适用于 User Input；
- `CrossSessionInputEnvelope.reply_route` 是跨会话入队请求携带的 `reply-to`：回答该请求及其后续报告
  最终应回复哪个 Channel，不是已经解析完成的投递目标。

合法组合：

| Input origin | cross-session binding | channel source |
| --- | --- | --- |
| User | None | Desktop 或 Device |
| Runtime | ControllerDelivery / ProxyReport | 无；回复路径由跨会话消息信封表达 |
| Runtime | None | 无；现有 Goal/Context continuation 继承所属 output cycle |

Device source 与任意 cross-session binding 同时出现必须拒绝；终端不能伪造主控投递。

### 4.3 Store 形状

`inputs.channel_source_json` 使用显式 tagged JSON。旧行 `NULL` 按 `InputOrigin` 解释：User Input 读取为
`Desktop { Text, Text }`，Runtime Input 读取为无外部 Channel；新 User Input 显式保存 Desktop 或 Device，新 Runtime Input
保持为空。

Store 反序列化后调用统一 `validate_input_message` 扩展校验。历史替换、Queue 取消、Fork、Goal held
恢复和 Session 删除继续沿既有 Input 生命周期处理该字段：

- Fork 复制前缀内 Input 时原样保留设备来源；
- 历史重入产生的新 Input 是当前 Desktop 来源，不继承旧设备来源；
- queued Device Input 被取消后与 Input 一起删除；
- Conversation clear 删除对应结构事实；
- 重命名设备不重写历史来源。

### 4.4 跨会话入队请求的回复路径

跨会话输入采用“公共信封 + 业务来源”的结构，避免在 `ControllerDelivery` 和 `ProxyReport`
两个 variant 中重复定义 `reply_route`：

```rust
pub struct CrossSessionInputEnvelope {
    pub binding: CrossSessionInputBinding,
    pub reply_route: ReplyRoute,
}

pub enum CrossSessionInputBinding {
    ControllerDelivery {
        controller_session_id: SessionId,
        controller_run_id: RunId,
        controller_tool_call_id: ToolCallId,
    },
    ProxyReport {
        source_session_id: SessionId,
        source_run_id: RunId,
        source_goal_id: Option<GoalId>,
        source_run_status: RunStatus,
    },
}

pub enum ReplyRoute {
    SessionDefault,
    Device {
        device_id: DeviceId,
        requested_output: OutputPreference,
    },
}
```

- Desktop 来源的跨 Session 入队请求使用 `SessionDefault`；
- Device 来源的跨 Session 入队请求使用 `Device`，冻结来源设备和输入偏好，不被之后的 Session
  默认托管切换改写；
- `ControllerDelivery` 入队时由信封附带 `ReplyRoute`；普通会话的 same-run/Context continuation
  在内存执行链中继承它；
- `GoalInputBinding` 增加可空 `reply_route`，只在该 Goal generation 由 `ControllerDelivery`
  触发时填写，后继 Goal Input/Run 原样复制，使进程恢复后仍能生成正确的报告路由；
- 来源 Run 不是 `ControllerDelivery`，例如运行中才开启代理，产生的 `ProxyReport` 使用
  `SessionDefault`；
- `ProxyReport` 可靠地进入目标 Session 时把该 `ReplyRoute` 作为自身入队请求的 `reply-to`，但不复制
  原始 client input ID、连接 ID 或音频状态。

生成 ProxyReport 时按“当前来源 Input 的 ControllerDelivery envelope → GoalInputBinding →
`SessionDefault`”的顺序解析。这不建立第二份消息或 Delivery Store，只是在既有 Input 上保存
入队链路后续需要原样传递的回复路径。本版本尚未发布，开发阶段产生过的旧字段名和旧 JSON
形状不作为兼容契约保留。

### 4.5 主控投递启动 Goal

主控工具 `send_session_message` 显式携带必填 `start_goal`：单轮任务传 `false`，需要跨多个 Run
持续执行的长任务传 `true`。该字段表达入队意图，不额外创建另一套跨会话命令类型。

当 `start_goal = true` 时：

- 目标必须是当前由该主控代理的普通 Session，Queue 为空且当前没有 Goal；
- 目标模型必须支持 Goal 所需的 Tool Call；
- Runtime 在同一 Store 操作中原子接受 `ControllerDelivery` Input、首次 Run、Goal 与
  `GoalInputBinding`，其中 binding 写入信封冻结的 `reply_route`；
- 后继隐藏 continuation 只复制 `GoalInputBinding.reply_route`，不复制首条消息的跨会话来源信封；
- Goal complete、blocked、预算暂停或失败链路结束后，最终 `ProxyReport` 重新形成自己的信封，
  并继续携带同一 `reply_route`；
- 同一主控 ToolCall 重放仍命中首次 Input/Run/Goal，不重复建立 Goal。

## 五、产品投影

`ConversationInputSourceSnapshot` 增量增加：

```rust
Desktop {
    modality: InputModalitySnapshot,
    requested_output: OutputPreferenceSnapshot,
}
Device {
    device_id: DeviceId,
    device_name: String,
    modality: InputModalitySnapshot,
    requested_output: OutputPreferenceSnapshot,
}
```

两个 variant 同时用于 `UserMessageSnapshot.source` 和 `QueuedInputSnapshot.source`。既有
`User` variant 作为旧数据/旧客户端兼容投影，等价于 `Desktop { Text, Text }`。Device name 在读取
投影时从 Registry 当前名称获得；若设备已撤销或历史数据只剩 ID，使用稳定的“已移除设备”回退，
不能把来源降级为 Desktop。

Assistant Message 不复制 source 字段，也不生成设备专用 Message。其所属 Run 已能通过 Input ID
回查来源；需要在产品层定位时扩展 Run/Conversation 投影的关联，而不是保存第二条回复。
Desktop UI 始终通过这份规范 Conversation 展示所有显式 User/Assistant 正文；附加渠道投递的
成功、失败或取消都不改变该可见性。

## 六、PC 输出托管

### 6.1 权威状态

PC 输出托管是 Controller Session 的可选权威字段：

```rust
pub struct PcOutputHosting {
    pub device_id: DeviceId,
}
```

本状态只表示“当前托管给谁”，路由、幂等与并发控制都不依赖变更时间，
因此不复制既有 `SessionProxyState.changed_at_ms`。若未来产品需要托管审计历史，
应增加独立的变更记录，而不是在取消托管时会被清空的当前状态上保存时间。

它与 v0.20.0 普通 Session 的 `SessionProxyState` 完全不同：

- proxy 表示普通 Session 的结果是否报告到主控；
- PC output hosting 表示 Desktop 从主控发起的回复是否投到一个终端。

不能复用同一个字段或 UI 开关。
“托管”只决定是否新增一条 Device 投递，不是把规范回复从 Desktop UI 迁走。

### 6.2 命令

建议增加：

```text
SetCurrentControllerOutputHosting {
  device_id: Option<DeviceId>
}
```

- `Some` 设置或切换；`None` 解除；不使用 toggle；
- Runtime 自行解析当前 Controller，请求不接受 Session ID；
- Device 必须 paired，允许暂时 offline；
- mutation gate 与 Store 单事务保证最后一次明确设置胜出；
- 重复设置相同目标幂等返回 `changed=false`；
- revoke device 在同一事务清除所有引用它的托管。

Runtime 的 `SessionSummary` 或 `SessionViewSnapshot` 只增加稳定托管投影：Device ID 和当前名称。
它不为了 UI 反向读取 Host 连接 Registry，也不注入广义的 `DeviceGatewayControl`。

Desktop 需要在线状态和 effective output preference 时，由 Host Application Facade 同时读取 Runtime 稳定
投影与 Gateway 瞬时 presence，以 Device ID 组合成响应 DTO。该 DTO 不持久、不回写 Session；Gateway
不可用时保留稳定托管目标，并将在线状态投影为 `unknown`，不删除托管也不伪装成已解除。

## 七、输出周期与附加投递

### 7.1 为什么不是简单 Run → connection

一个用户交互可能经历：

- 同一 AgentExecution 的多个 Tool step；
- `load_skill` 等同 Run continuation；
- Context compaction 后继续同一 Run；
- Goal settlement 创建下一 Runtime Run；
- 审批等待后恢复；
- 模型可能跨多个 step/continuation 串行调用多次 `speak`。

Runtime 需要一个轻量、易失的 `OutputCycleCoordinator`，追踪来源、是否已播报以及是否已经补齐过；
它不保存播报文本副本，不成为第二份持久会话。

### 7.2 Output Cycle

```rust
pub struct OutputCycleState {
    pub source_input_id: InputId,
    pub has_speech: bool,
    pub speech_segment_count: usize,
    pub speech_reminder_issued: bool,
    pub pending_assistant_text: Option<String>,
}
```

每次合法 `speak(text)` 立即通过 Host 输出端口交付，成功后设置 `has_speech` 并累计当前周期
成功片段数；计数只用于限制每周期最多 20 段，不保留片段历史或版本。
`pending_assistant_text` 只在最终正文已完成、但需要隐藏补齐续跑时暂存该正文，不派生新的公开回复。

- 普通 Desktop/Device Input 被领取时开始一个 cycle；
- `ProxyReport` 被主控领取时开始新的报告 cycle，`source_input_id` 指向该报告 Input；
- `ControllerDelivery` 在目标普通会话中不直接开始外部 output cycle，它只在入队 binding 中携带
  `ReplyRoute`，供后续 `ProxyReport` 入队时继续传递；
- 同 Run continuation 复用原 cycle；
- Goal `Continue` effect 把 cycle 交给后继 Goal Input/Run；
- Goal complete/blocked/budget stop、普通 Completed、Failed 或 Cancelled 结束 cycle；
- 审批等待不结束 cycle；Queue 中后续独立用户 Input 不加入当前 cycle；
- Runtime 重启不恢复 cycle、Host 播放队列或尚未执行的隐藏补齐，也不补播；规范 Conversation
  和已持久的 Tool 事实仍完整。

Runtime 同一 Controller Session 仍只执行一个改变 Conversation 的父 Run，因此 cycle 可以跟随活动
输入链，不需要并发 Message Bus 或全局路由表。

### 7.3 从来源 Input 解析附加投递

Output cycle 不复制一份路由状态。最终结算时，规范 Assistant Message 已经保存并对
Desktop UI 可见；Runtime 只通过 `source_input_id` 与既有 binding 解析零到多个附加投递：

```text
User Input / Device source
  → deliveries = [来源 Device ID + 冻结 requested output]

User Input / Desktop source
  → 读取此刻 Controller 的 PC output hosting
  → 无托管：deliveries = []
  → 有托管：deliveries = [该 Device ID + 使用设备当前合法偏好]

Runtime Input / ProxyReport
  → 读取 CrossSessionInputBinding.reply_route
  → Device：deliveries = [来源设备 + 冻结偏好]
  → SessionDefault：每段输出投递时读取目标 Session 当前默认托管，有托管则生成对应投递，否则为 []
```

上述 `[]` 只表示没有附加渠道投递，不表示没有输出；规范 Assistant Message 仍在 Desktop UI。
未来 Desktop 音频或 IM 接入时可以增加对应 delivery，不把这个集合改回单一目标。

同 Run 的 Tool/Context/Goal/Speech continuation 始终保留该 output cycle 的根 `source_input_id`，
不会创建后继 Runtime Input。直接 Desktop Input 和没有明确外部返回渠道的 ProxyReport 都按
`SessionDefault` 规则处理。每次 `speak` 入队和最终文字投递都重新读取当前托管，因此中途切换可影响
尚未形成的后续分段，同时不会改写明确的来源设备。若 Desktop 在后续分段形成前解除托管，
该 PC 轮次或代理报告的后续内容不再向设备投递，
但 Desktop UI 中的规范内容不受影响。

## 八、Channel 有效输出与降级

Runtime 先形成附加 delivery 集合，Host 再对每个 delivery 独立将业务偏好与当前可用能力取交集。
下表描述单个 delivery 的有效输出：

| 请求 | text 可用 | audio 可用 | 结果 |
| --- | --- | --- | --- |
| text | 是 | 任意 | text |
| text | 否 | 任意 | unavailable |
| audio | 任意 | 是 | audio |
| audio | 任意 | 否 | unavailable，不自动改成 text |
| text_and_audio | 是 | 是 | text_and_audio |
| text_and_audio | 是 | 否 | text + audio unavailable 状态 |
| text_and_audio | 否 | 是 | audio + text unavailable 状态 |
| text_and_audio | 否 | 否 | unavailable |

业务偏好为 `audio` 时，不因 TTS 失败静默把完整长文改成文字；选择
`text_and_audio` 时，其中一种能力失败不应抹掉另一种已成功输出。
一个 delivery 失败也不能取消其他 delivery，更不能改写已结算的 Conversation。

- Device 的 text/audio 可用性来自 WSS 协商能力、Host ASR/TTS 可用性与服务端策略；
- Desktop UI 的规范文字展示不是 delivery capability，它始终由 Conversation/SSE 保证；
- 未来 Desktop 语音模式由本地媒体 Adapter 增加可选音频 delivery，并复用同一
  Host ASR/TTS 与上表，不新建 Desktop 专用降级规则。

有效能力和失败状态由每个 delivery Adapter 使用自己的传输协议返回：Device 使用 WSS
控制消息，Desktop 音频未来使用本地客户端协议。投递决策本身不依赖 WSS、SSE 或具体音频传输。

PC output hosting 目标离线时，该附加 delivery 失败；完整回复仍在 Desktop UI。
不自动改投其他设备，也不建立离线队列。

## 九、可靠结算与输出端口

### 9.1 端口

`assistant-runtime` 定义基础设施中立的端口：

```rust
pub trait ChannelOutputDispatcher: Send + Sync {
    fn dispatch(&self, output: ChannelOutput) -> ChannelOutputFuture;
    fn dispatch_speech(&self, segment: ChannelSpeechSegment) -> ChannelOutputFuture;
    fn requires_speech(&self, deliveries: Vec<ResolvedChannelDelivery>) -> ChannelSpeechRequirementFuture;
}

pub struct ChannelOutput {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub assistant_text: Option<String>,
    pub speech_completed: bool,
    pub deliveries: Vec<ResolvedChannelDelivery>,
}

pub struct ChannelSpeechSegment {
    pub session_id: SessionId,
    pub run_id: RunId,
    pub segment_id: ToolCallId,
    pub text: String,
    pub deliveries: Vec<ResolvedChannelDelivery>,
}
```

该端口只由 Host 实现；Runtime 单元测试使用 fake。`ChannelOutput` 不含连接 ID、PCM、Provider、
WebSocket sender 或重试策略。

`ResolvedChannelDelivery` 是 Runtime 在本次投递前从来源 Input、`ReplyRoute` 和当前 Session
托管关系解析出的附加渠道目标，携带稳定目标及具有明确业务语义的偏好来源：
明确来源轮次使用 Input 冻结偏好，主控默认投递使用目标 Channel 当前预设。
本版实现 Device delivery；未来 Desktop 音频、IM 等作为新 delivery variant 增加。
Host Dispatcher 对集合逐条解析 Adapter 与有效能力，避免 Runtime 依赖 Gateway、Desktop 媒体或 IM 在线状态。

### 9.2 调用顺序

```text
Recorder 可靠保存 Tool Call 并标记 speak started
→ speak Tool 解析当前 delivery
→ dispatch_speech 立即合成并加入 Host FIFO
→ Tool Result 进入规范 Conversation
→ AgentExecution 候选结束时依次经过 Goal 与 Speech 门禁
→ 需要语音但未播报：在活动 Run 内可靠追加一次带 speech_delivery_reminder 身份的隐藏消息
→ 相同 RunId 启动下一次 AgentExecution
→ 最终 dispatch 完整文字与 speech_completed 状态
```

调用输出端口不能持有 Session mutation gate。Dispatcher 接受后由 Host 自己持有任务句柄和取消令牌；
错误写入结构化诊断并通过设备连接返回，但不修改已完成 Run。

Runtime Event 可以增加 `ChannelDeliveryStateChanged` invalidation 供 Desktop/H5 观察，但不能把事件当作
实际音频投递。首版不把每个音频帧、分段正文或 Host FIFO 放入公共 SSE。

### 9.3 进程关闭

- HostSupervisor 先撤回 mDNS、停止新的 Desktop 变更命令和 WSS 连接，保留已建立连接用于短暂排空；
- Runtime 停止接收新 Input 并按既有策略结算或取消活动 Run；
- Device Dispatcher 停止接收新 output，取消 ASR/TTS/播放任务；
- 允许已发送的 `playback_end(shutdown)` best-effort 完成；
- HostSupervisor 等待各 task group 在明确 deadline 内结束，不等待设备无限 drain，也不把未播放内容持久化；
- Runtime 完成 shutdown，Store 最后 flush/join，再清理 listener 与发现/endpoint 文件。

## 十、竞态矩阵

| 竞态 | 决策 |
| --- | --- |
| Device Input accepted 后设备断线 | Run 继续；规范内容仍在 Desktop UI，设备 delivery 不补播 |
| Device Input 执行中 PC 托管改变 | 无影响；附加 delivery 仍只返回来源 Device |
| Device A 发起主控投递，报告时 PC 托管 B | 附加 delivery 返回 A，不增加 B |
| 无明确外部渠道的 ProxyReport，报告时 PC 托管 B | 使用主控默认规则，增加 B delivery |
| PC Input 执行中切换托管 A→B | 已经由 `speak` 加入 A 队列的片段不迁移；切换后的 `speak` 与最终文字投递使用 B |
| PC Input 结算前解除托管 | 不产生附加 delivery；Desktop UI 内容仍完整 |
| `speak` 已入队后切换托管 | 已形成的 delivery 和播放队列不回收；新设置只影响之后尚未形成的 delivery |
| 目标设备被 revoke | Store 清托管；已形成但未发送的 delivery 在 Host 认证检查时拒绝，Desktop UI 不受影响 |
| 同设备新连接替换旧连接 | 只向新 connection owner 发；旧连接关闭 |
| playback 中设备发起新输入 | Host 先取消当前播放，再接受新输入；旧 Run 事实不变 |
| 审批等待 | 不播放；设备显示需在 Desktop 操作 |
| Goal Continue | 保留同一 Run/cycle 的 `has_speech`，下一 AgentExecution 的 `speak` 继续追加 FIFO |
| Runtime 重启 | 不恢复输出 cycle、隐藏补齐或播放队列，不补播；Desktop 从权威 Conversation 恢复 |

## 十一、测试要求

Runtime 单元/契约测试至少覆盖：

- Desktop 与 Device Input 的合法/非法来源组合和旧行缺省；
- Host 本版把 Device 路由到 Controller；Runtime 通用入口也可接受 Host 选择的其他合法 Session，
  idempotency 命中返回同一 Input/Run；
- Queue、历史分页、Around、Fork、reenter、clear 对来源的正确保留或重建；
- PC 托管设置、切换、解除、revoke 原子联动和重启恢复；
- 所有 Channel 的显式输入和最终 Assistant 正文在 Desktop UI 始终可见，不受 delivery 成败影响；
- Device source 优先于托管；PC source 结算时读取最新托管，Host dispatch 时读取目标设备当前合法偏好；
- delivery 集合允许零到多条，单条失败不影响其他 delivery 或规范 Conversation；
- Device 来源的 ControllerDelivery 将 report route 传到 ProxyReport，跨 Goal continuation 不丢失；
- 无明确外返回渠道的 ProxyReport 使用结算时的 PC 托管，不猜测最后活跃设备；
- 多次 `speak` 按调用顺序 dispatch，Tool/Context/Goal continuation 不会覆盖已入队片段；
- 未调用 `speak` 时的隐藏补齐只执行一次，且不替换完整 Assistant 正文；
- Dispatcher 失败不改变 Run status，且 mutation gate 已释放；
- 多设备并发连接不会把 A 的 output 发给 B。
