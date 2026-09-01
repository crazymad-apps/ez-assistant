# v0.21.0 技术方案附件 C：ASR、播报 Tool 与 TTS

## 一、文档定位

- 状态：已确认（2026-08-27）
- 上位方案：[`v0.21.0 技术方案`](技术方案.md)
- 约束范围：Host 语音 Provider 边界、上行 ASR、播报 Tool、分段 `speak` 有界排队、下行 TTS、播放取消
  和开发音频留存

本附件不改变完整 Assistant Message：模型正常回答始终进入规范 Conversation；播报文本只是同一
Conversation 中的 Tool Call/Result 事实，用于设备音频输出。

## 二、语音 Provider 架构

### 2.1 两个平行端口

ASR 与 TTS 不能绑定成一个服务商大接口：

```rust
pub trait SpeechRecognizer: Send + Sync {
    fn recognize(&self, request: AsrRequest) -> AsrFuture;
}

pub trait SpeechSynthesizer: Send + Sync {
    fn synthesize(&self, request: TtsRequest) -> TtsFuture;
}
```

```rust
pub struct AsrRequest {
    pub pcm: Pcm16Mono16Khz,
    pub language_hint: Option<String>,
    pub cancellation: CancellationToken,
}

pub struct TtsRequest {
    pub text: String,
    pub voice: String,
    pub output: PcmFormat,
    pub cancellation: CancellationToken,
}
```

端口与首个 `DashScopeSpeechRecognizer`、`DashScopeSpeechSynthesizer` 均放在
`runtime-host` 私有模块。Runtime 只知道 ASR 后的可信文字和 TTS 前的 output intent，不依赖 Provider。

### 2.2 首版 Provider

首版复用 `agent-robot` S3.0 已验证的阿里百炼/DashScope 路线：

- ASR：整句 PCM 上传，获得最终 transcript；
- TTS：每次提交一个最多 120 个 Unicode 字符的自然播报分段，取得 PCM16/16 kHz/mono；
- HTTP 由现有 `reqwest` 基础设施承载；
- Provider endpoint、模型、音色、credential 和超时从已校验配置构造；
- Provider 原始 body、request ID、credential 和音频不进入普通 Runtime 错误或日志。

首版有意采用整句 ASR 和分段 TTS，先保证话语边界、幂等、多次 `speak` FIFO 和播放正确性。未来改为流式
Provider 只替换 Host 端实现，不改变终端 WSS、Runtime Input 或播报 Tool 契约。

### 2.3 可用性编译

Host 启动/reload 后分别得到：

```text
ASR: unavailable | ready | degraded
TTS: unavailable | ready | degraded
```

- 配置缺失/非法：unavailable，不阻止 Runtime 和文字终端启动；
- Provider 临时失败：单次请求失败，保持可重试诊断；不静默改写配置状态；
- capability hello 只公布当前已配置且可构造的能力；
- 连接验证是显式用户动作，可能产生真实费用，不能在每次启动自动请求 Provider。

## 三、上行话语与 ASR

### 3.1 本版 Device Host 状态

每设备同一时间最多一个上行话语：

```text
Idle
  → ReceivingPcm
  → Recognizing
  → SubmittingInput
  → Accepted
  → Idle

ReceivingPcm/Recognizing/SubmittingInput
  → Cancelled | Failed
  → Idle
```

状态只存在于 Device Adapter 连接/媒体 owner，不进入 Runtime Run 状态。Host Router 将结果归一化为
`SubmitSessionInputRequest` 并调用 `submit_session_input` 后，Runtime 才接管可靠业务生命周期。

### 3.2 `listen_start`

Host 在接收 PCM 前核对：

- 连接已认证且设备仍 paired；
- effective capability 包含 PCM input；
- 当前没有正在采集的上行话语；上一分段的 ASR 可以仍在运行；
- `client_input_id` 未在当前连接中以另一内容使用；
- PCM 格式精确为 protocol 1.0；
- requested output 在设备能力范围内；
- 当前没有正在播放的音频；若有则先取消播放并完成状态切换。

成功后分配/核对 stream ID、初始化 sequence、字节和时长计数器，并返回 listening 状态。

### 3.3 PCM 接收与 `listen_stop`

- 每帧在进入缓冲前校验 header、方向、sequence 和大小；
- 使用有界内存/临时文件 collector，不能把整句音频复制到多个无界 `Vec`；
- 超过最大时长、字节或静音空话语时结束并返回稳定错误；
- `listen_stop` 必须带最后 sequence，Host 核对完整后才封口；
- `listen_cancel` 直接丢弃 collector，不调用 ASR 或 Runtime。

开发模式可以边收边把上行 PCM 写入临时目录；Release collector 只保留完成 ASR 所需的短期数据，
请求完成后释放。

### 3.4 ASR 与 Input 提交

```text
sealed PCM
→ Host 接管 PCM 并发送 input_segment_accepted
→ SpeechRecognizer::recognize（分段可并行）
→ trim + 非空/长度/UTF-8 校验
→ 向设备发送分段 transcript（若 capability 允许）
→ 等待 2 秒连续输入窗口结束且全部 ASR 完成
→ 按录音顺序以换行合并 transcript
→ 派生 idempotency key
→ Host Channel Router 从 Runtime 权威 Session 投影选择目标 Session
→ AssistantRuntime::submit_session_input
→ 收到首次或幂等 AcceptedInput
→ input_accepted
```

`input_segment_accepted` 只转移该段 PCM 的保管责任；只有 Runtime 返回接受成功才发送最终
`input_accepted`。ASR 空结果、不可用、超时、取消或 Provider 失败的分段不进入合并；整轮没有有效
transcript 时不创建 Input。

PTT 事件到达 Host 时立即介入：若上一轮还未请求 Runtime，暂停提交计时并归入同一逻辑输入；若
Runtime 已有活动 Run，则取消该 Run、取消剩余播报并建立新聚合轮次。迟发下行不得在采集中恢复播放。

ASR transcript 原样成为规范 UserMessage 文本，但进入 Runtime 前执行产品已有的非空和长度约束；
不在 Host 用 LLM “润色”或改写识别结果。

### 3.5 Desktop 后续兼容

Desktop 语音输入不使用设备配对、Device ID 或 WSS 话语状态。后续由 Host 增加本地媒体
Adapter，以已认证 Desktop Client 和目标 Session 接收流式上传，封口后复用同一
`SpeechRecognizer`。识别成功后以
`InputChannelSource::Desktop { modality: SpeechTranscript, requested_output }` 进入通用 Session
Input 受理链。当前 Desktop 本版仍固定提交 `Text + Text`，不在本附件中提前实现麦克风 UI。

## 四、播报 Tool

### 4.1 装配条件

所有可能形成主控 Channel 音频输出的 Controller output cycle 都获得 `speak` Tool，包括真实用户 Input 与
`ProxyReport`。代理报告被主控领取时建立新的报告 cycle：有明确来源 Device 则增加返回该设备的
delivery，否则使用主控默认规则并在结算时读取 PC 输出托管。同一 cycle 的 Skill、Context 和 Goal continuation
继续装配同一个工具，并共用 cycle 内的“是否已成功播报”状态。`ControllerDelivery` 在目标普通会话中
不直接建立外部输出 cycle。

这是为了同时满足 ToolSet 冻结和“执行中的 PC 托管切换可以影响尚未投递的当前轮次”。Runtime
在每次 `speak` 执行时解析当前目标，Host 再解析该目标的当前能力；没有需要音频的 delivery 时该次 Tool 明确失败，
不生成假的已播报状态。

工具集合仍在每个 Run 构造时冻结，不在执行中动态插入或移除 Tool Definition；
路由和媒体能力则在每次实际 `speak` 时判定。已接受的片段立即进入当时目标的队列，
后续托管切换不迁移或回收它。

Device 输入同时在本轮规范 UserMessage 中携带模型可见、产品隐藏的 `channel_input` 内部 Part，冻结来源、
输入模态和回复偏好。`audio` / `text_and_audio` 明确要求模型调用 `speak`，`text` 明确不为渠道投递调用；
这样 Tool 是否使用由本轮可信输入事实驱动，而不是让模型从正文猜测。Desktop 输入按产品要求不增加该
Part；结算时才变化的 PC 输出托管仍沿用既有 route 规则，不把易失托管状态伪装成输入事实。

### 4.2 Tool 定义

模型可见定义建议：

```text
name: speak
description:
  Speak one concise, natural segment for the current output immediately.
  Each call is at most 120 Unicode characters. Split longer speech into sequential calls only at
  natural sentence or semantic boundaries; calls are played in exact order.
  Do not include device identifiers, segment labels, or repeated overlap.

input:
  text: non-empty string, bounded
```

Tool 不接受：

- Device ID、Session ID、connection ID；
- voice/model/provider；
- “立即播放”或延迟；
- 任意 URL、SSML、文件路径；
- 是否覆盖的布尔值。

目标和音色由上层决定。首版只接受纯文本，避免模型注入服务商专用 SSML。

### 4.3 执行语义

`speak` 是 Runtime 私有的 Host 交付 Tool：

1. 解析并校验非空、最多 120 个 Unicode 字符和控制字符，同时确认当前 output cycle
   尚未成功接受 20 个片段；
2. Recorder 先按现有边界保存 Tool Call 并标记该调用 started；
3. Runtime 根据 output cycle 解析附加 delivery，调用 Host `dispatch_speech`；
4. Host 预留有界 FIFO 位置、调用 TTS，并将成功音频绑定到该队列位；
5. Host 接受成功后才把 output cycle 标记为已播报，工具返回 `{ accepted: true }`；
6. 目标离线、不需要音频、TTS 失败、队列满或第 21 次调用均返回明确 Tool Error；第 21 次
   调用不会进入 Host TTS。

该 Tool 使用 Runtime 私有授权 facts 直接允许，不进入文件/Shell 权限规则或 Desktop 审批。

## 五、分段 `speak` 与结束补齐

### 5.1 输出周期状态

每个 output cycle 只保留来源 Input、`has_speech`、成功片段计数、`speech_reminder_issued`
和必要的原始最终正文。
Runtime 不建立 `SpeakCandidate`、revision、播报历史或音频持久表。Conversation 仍按既有
Agent Recorder 记录 Tool Call/Result，Host FIFO 只是当前连接的易失交付状态。

### 5.2 交付边界

- `TextDelta`、`ToolProposed` 或普通 Assistant 文本不触发 TTS；
- 只有已越过 Recorder tool-started 可靠边界的 `speak` 执行才能调用 Host；
- 调用时必须存在需要 audio 的合法 delivery，否则工具返回错误；
- pending approval 本身不播放，审批后模型真实调用 `speak` 时才入队；
- Goal/Context/Skill continuation 在同一 Run 的后续 AgentExecution 中继续调用并追加分段；
- 已入队分段不因当前 Run 后续 Failed、Cancelled 或 Interrupted 而撤回。

### 5.3 真正结束

当前 AgentExecution 已形成候选 Assistant Message，且前置 Context/Goal 门禁均放行时：

1. 若整个 cycle 已成功调用过 `speak`，直接关闭 cycle；
2. 若当前没有任何有效 audio delivery，直接关闭 cycle；
3. 否则在活动 Run 内可靠追加候选 Assistant Message，以及一条带 `speech_delivery_reminder`
   InternalContext 的隐藏 Runtime UserMessage；
4. 使用相同 RunId 启动下一次 AgentExecution；结束后不再追加第二条提醒，仍无 `speak` 则向设备返回
   `no_speak_text`。

提醒消息使用隐藏 Runtime UserMessage 和 `speech_delivery_reminder` InternalContext，明确告知模型该消息
不是真实用户交互；不得道歉、问候、确认提醒或重新生成普通正文，只需自然地调用 `speak`。

### 5.4 恢复与失败

- 不把完整 Assistant Message 静默送 TTS，不做 Host 字符串硬截断；
- 已被 Host 接受的分段不因后续 Run 失败而撤回，因为用户可能已经听到；
- Runtime/Host 重启不恢复 output cycle、播放队列或未完成的 AgentExecution，不补播旧音频；
- 恢复时已落账但未完成的 Tool 仍按既有不可重放副作用边界收敛。

## 六、TTS 与下行播放

### 6.1 调度

Host `ChannelOutputDispatcher` 每收到一个 `ChannelSpeechSegment` 后：

```text
resolve every delivery adapter and effective capability
→ 为需要 audio 的在线目标预留 FIFO 位置
→ SpeechSynthesizer::synthesize(segment.text)
→ 校验 PCM，并将结果填入对应队列位
→ connection writer 严格按位置顺序播放
```

上述是附加投递；Desktop UI 的规范文字早已由 Conversation/SSE 可见，不作为其中一条互斥
delivery。完整文字对 Device delivery 是 `text_output`。本版 Device Adapter 把音频播放实现为
`playback_start` → 20 ms frames with bounded queue →
`playback_end(completed)`。未来 Desktop Adapter 接收同一 PCM 结果并交给本地播放端，不改
`speak` 分段、TTS 端口或有效输出规则。
每条 delivery 独立报告状态；一条失败不取消其他 delivery，也不改变规范 Assistant Message。

`text_and_audio` 的文字可以先发；TTS 失败只让音频部分失败。`audio` 模式下完整 Assistant text 仍在
Desktop Conversation，但不自动下发终端。

### 6.2 PCM 校验与发送

- Provider 结果必须被解码/校验为 PCM16 LE、16 kHz、mono；
- 若 Provider 返回 WAV，Host 解析 header 后只发送 PCM payload，不能把 WAV header 当样本；
- 非 20 ms 尾段可在 Host 补零到完整 640-byte frame，并在 `playback_start` 提供原始 sample count，
  设备只播放有效 samples；
- 每个 output 分配唯一 output ID 和 stream ID，sequence 从 0 开始；
- 下行 sender 使用有界队列，设备持续落后时取消播放并返回 `playback_end(backpressure)`。

### 6.3 播放取消

取消来源：

- 设备 `playback_cancel`；
- 同设备开始新的文字/语音输入；
- connection replaced/disconnected；
- Host shutdown。

取消令牌同时停止 TTS HTTP 请求（若仍在合成）、PCM 分帧和 writer。`playback_end(cancelled)` 只在
连接仍可写时 best-effort 发送。取消播放绝不调用 Runtime `CancelRun`。

### 6.4 TTS 失败

Provider 配置、认证、超时、协议、解码和输出超限映射到稳定设备错误：

```text
tts_unavailable
tts_auth_failed
tts_timeout
tts_provider_failed
tts_invalid_audio
tts_output_too_large
```

公共错误不携带 Provider body。Run 保持 Completed；设备端收到稳定错误，Desktop 仍保留完整正文，
不把投递失败伪装成 Assistant 失败消息，也不为它恢复已移除的右侧托管卡片。

## 七、开发音频留存

### 7.1 开关

Host 提供仅开发/测试使用的进程配置：

```text
speech.debug_audio_directory = <absolute path> | absent
```

- 缺省和 Release 均为 absent，不保存；
- 配置路径必须是明确绝对目录，不能默认写入 Workspace 或用户项目；
- 不新增数据库、Runtime DTO、Desktop 管理 UI、TTL、容量或自动清理。

### 7.2 文件

建议最小命名：

```text
<timestamp>-<device-short>-<client-input-short>-uplink.pcm
<timestamp>-<device-short>-<output-short>-tts.pcm
```

必要时旁置一个不含正文的简短 JSON 元数据，记录 sample rate、channels、bytes、frames 和稳定错误。
写入失败只影响开发对比文件，不改变 ASR/TTS/Run 结果；普通日志仅记录路径和错误摘要。

## 八、最小配置方案

本版确认使用 Runtime Home `config.toml` 的最小配置：

```toml
[speech.asr]
provider = "dashscope"
model = "..."
credential = "..."
endpoint = "https://dashscope.aliyuncs.com" # 可省略，默认使用该地址
timeout_ms = 30000

[speech.tts]
provider = "dashscope"
model = "qwen-audio-3.0-tts-flash"
voice = "longanhuan_v3.6"
credential = "..."
endpoint = "https://dashscope.aliyuncs.com" # 可省略，默认使用该地址
timeout_ms = 30000
```

实际字段复用现有配置加载、凭据脱敏投影、校验和显式 reload 机制。本版不增加 Desktop
设置表单、公共配置写入 DTO 或新 revision 字段；Desktop 只显示 `ready | unavailable | degraded`
与必要的脱敏诊断。ASR 与 TTS 保持平行 Provider SPI，首个 Adapter 为 DashScope。
本附件不把示例模型/音色冻结为最终产品默认值。
M5 只构造 ASR Adapter；`[speech.tts]` 即使结构合法也保持 unavailable，直到 M6 的真实 TTS Adapter
可以构造并完成输出校验，不能把“存在配置”当成能力可用。

## 九、测试矩阵

### 9.1 ASR

- 完整 PCM → transcript → 同一个 Device Input；
- 空话语、坏 PCM、超长、ASR timeout/取消/鉴权失败不创建 Input；
- `input_accepted` 丢失后相同 client input 重试返回首次 Run；
- 开发文件开启/关闭不改变 transcript 和 Runtime 结果。

### 9.2 Speak 分段与补齐

- 一次调用采用 A；A→B→C 按 A、B、C 串行播放；
- 超过 120 个 Unicode 字符、空文本或含非法控制字符时 Tool 拒绝；
- Tool started 可靠提交失败时不触发 TTS；
- skill/context/Goal continuation 共用同一 Run/output cycle 并按 Tool Call 顺序追加队列；
- 有效 audio delivery 但本轮未调用 `speak` 时只进行一次隐藏补齐；补齐仍遗漏时不循环；
- 隐藏提醒不出现在 Desktop 正文，模型不道歉、不回应提醒，最终文字仍使用补齐前的完整 Assistant Message；
- ProxyReport 建立独立报告 cycle；明确 Device 返回渠道优先于 PC 托管，无明确渠道时使用 PC 托管；
- 后续 Failed/Cancelled/Interrupted 不撤销已入队分段；
- Runtime 重启不补播。

### 9.3 TTS/播放

- text/audio/text_and_audio 三种偏好；
- WAV/PCM 正确解析、尾帧、sequence、取消和慢设备背压；
- TTS 失败不改 Completed Run；
- 新输入取消播放但不取消历史 Run；
- 连接替换只允许新连接收到后续帧；
- A/B 两设备并发 output 不串流。
