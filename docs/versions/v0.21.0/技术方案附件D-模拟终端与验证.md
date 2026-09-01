# v0.21.0 技术方案附件 D：Node/H5 模拟终端与验证

## 一、文档定位

- 状态：已确认（2026-08-27）
- 上位方案：[`v0.21.0 技术方案`](技术方案.md)
- 约束范围：Node 智能终端模拟器、H5 交互页、本地浏览器桥、协议测试对端、故障注入和版本验收

Node 进程是本版本真正的智能终端；H5 只是方便用户操作和监控 Node。所有被计入验收的终端行为
必须经过 Node 的 mDNS/WSS/设备 codec，H5 不能直连 Runtime Command、SSE 或数据库。

## 二、物理结构

```text
Browser H5
  │ loopback-only simulator control WebSocket
  │ microphone PCM / text / UI actions / playback PCM
  ▼
Node Device Simulator
├── Device Identity Store
├── mDNS Browser
├── Device WSS Client
├── JSON/PCM Codec
├── Reconnect + Device State Machine
├── Local H5 Server
└── Diagnostic Ring Buffer
  │
  │ formal device WSS
  ▼
Runtime Host Device Adapter
```

Node 只模拟终端边界，不装配 Agent、ASR、TTS 或 Runtime Client。ASR 必须发生在 Host；H5 的文字输入
是一种正式 `input.text` capability，而不是向 Host 注入 ASR 结果。

## 三、仓库与工具形态

本版本已实现独立开发工具：

```text
tools/device-simulator/
├── package.json
├── tsconfig.json
├── vite.config.ts
├── src/
│   ├── node/
│   │   ├── main.ts
│   │   ├── config.ts
│   │   ├── identity.ts
│   │   ├── discovery.ts
│   │   ├── connection.ts
│   │   ├── protocol.ts
│   │   ├── media.ts
│   │   ├── state.ts
│   │   └── webBridge.ts
│   └── web/
│       ├── main.ts
│       ├── store.ts
│       ├── audioCapture.ts
│       ├── audioPlayback.ts
│       ├── pcm-worklet.ts
│       └── styles.scss
└── tests/
```

该工具不加入 Cargo workspace，使用 TypeScript、Vite、`ws` 和 `bonjour-service`，Node 版本固定为
24。H5 使用原生 TypeScript，不复制 Desktop Store/组件，也不把模拟器 UI 提升为正式产品页面。

实现前需要新增 `docs/modules/device-simulator.md` 模块约束，明确它是开发验证宿主而非产品进程。

## 四、Node 权威状态

Node 模拟一台设备的持久与易失事实：

### 4.1 持久设备数据

```text
simulator-home/
└── device.json
    ├── simulator_device_installation_id
    ├── pending/active pairing credential（可空）
    │   ├── private_key
    │   ├── public_key
    │   ├── paired Device ID
    │   └── pinned Host installation/fingerprint
    └── display/default capability preset
```

- 文件写入使用用户明确指定或工具临时目录，不默认落入 Runtime Home；
- 未配对状态不预先生成或持久化长期设备密钥；SPAKE2 双向验证通过后才按附件 A 生成 pending
  credential，收到 `pairing_complete` 或以 pending credential 认证成功后再标记为 active；
- 私钥不发送到 H5、不打印日志；
- “重置设备”显式删除该模拟设备安装身份及 pending/active credential，相当于恢复出厂；新的长期
  key 仍只在下一次配对的 SPAKE2 双向验证通过后生成；
- “解除配对”是 Desktop/Host 操作，Node 收到 revoked 后清除 Host pairing 事实但可保留本地 key。

### 4.2 易失状态

```text
discovering
pairing
connecting
idle
listening
recognizing
accepted_or_queued
processing
needs_desktop_approval
synthesizing
speaking
error
```

Node 根据正式设备控制消息推进状态。H5 不自行根据按钮点击把 processing 标成 completed；连接断开后
由 Node 进入 reconnecting/idle/error。

## 五、H5 与 Node 本地桥

### 5.1 安全边界

- H5 server 只绑定 `127.0.0.1` 动态或显式端口；
- 启动时生成短期页面 token，通过首次 HTML bootstrap 注入或 same-origin cookie 使用；
- WebSocket upgrade 校验 Host/Origin 和 token；
- 不允许外部 LAN 浏览器控制模拟器；
- H5 不显示设备私钥、Host credential 或完整签名。

该本地桥是 simulator 私有协议，不复用正式设备 envelope，避免浏览器操作消息污染设备 wire。

### 5.2 H5 到 Node

最小交互：

```text
select_host
open_pairing
submit_text
start_microphone
microphone_pcm_chunk
stop_microphone
cancel_microphone
set_output_preference
cancel_playback
disconnect
reconnect
reset_device
set_capability_preset
inject_fault
```

`submit_text` 由 Node 编码为正式 `text_input`。麦克风数据由 H5 采集后交给 Node，Node 再按正式
16-byte header 编码为设备 PCM；浏览器不能直接向 Host WSS 写帧。

### 5.3 Node 到 H5

Node 推送当前状态快照和有界诊断事件：

- mDNS Host 列表、选择和 TLS 指纹；
- 配对码、pending/complete/revoked；
- 连接、capability、output preference；
- transcript、Input ID、Run ID、Queue/processing 状态；
- text output、最终播报文本、TTS/播放状态；
- PCM 上下行 frame/byte、sequence、buffer 水位和阶段耗时；
- 最近稳定错误码与 recoverable 标记。

诊断使用固定容量 ring buffer，例如最近 1,000 条；H5 长时间打开不会无限增长 DOM 或 Node 内存。

## 六、浏览器麦克风

### 6.1 采集

H5 使用 Web Audio API：

```text
getUserMedia
→ AudioContext
→ AudioWorklet
→ mono Float32
→ 确定性重采样到 16 kHz
→ 饱和转换 PCM16 little-endian
→ 20 ms / 640-byte chunks
→ local bridge
```

不使用 `MediaRecorder` 作为正式输入，因为其常见输出是 Opus/WebM，不能验证 protocol 1.0 的 PCM。
浏览器实际 AudioContext sample rate 可能为 44.1/48 kHz，必须测试重采样的累计样本数、边界和幅度。

PTT 行为：按下开始、松开停止；失焦、页面关闭、权限撤销或 worklet 失败必须发送 cancel/结束本地
采集，不能留下 Node 永久 listening。

### 6.2 Node 分帧

H5/Node 本地桥可以发送较大 PCM chunk，但 Node 必须重新切分为严格 640-byte正式帧并维护 sequence。
尾部不足一帧可补零，同时用 `listen_stop.last_sequence` 和采样计数保持可验证边界。

## 七、浏览器播放

Node 收到正式 `playback_start` 和 PCM frame 后，通过本地桥把 PCM 与 stream 元数据送 H5：

```text
PCM16 bytes
→ Float32 conversion
→ 跨传输帧连续重采样到 AudioContext sample rate
→ 单一 AudioWorkletProcessor 有界队列
→ AudioWorkletNode / AudioContext output
```

- H5 只播放 Node 已接受并校验的正式下行帧；
- Worklet 在音频渲染线程执行 120 ms 首缓冲和 80 ms 下溢恢复，主线程定时器不承担播放时钟；
- 相邻 `speak` 片段保持同一 Worklet 时间轴并加入约 160 ms 气口；完成状态等待对应 marker
  真正排空。本地容量或处理器故障明确显示，但不得自动通知 Node 取消整条 Host 待播队列；
- 用户停止播放先停本地音频，再由 Node 发送正式 `playback_cancel`；
- 新输入开始时同样执行取消；
- 浏览器 autoplay 限制通过用户首次交互解锁 AudioContext，不能把解锁失败误判为 Host TTS 失败。

## 八、H5 页面信息架构

本版 H5 以验证效率为目标，不追求真实终端外观。建议分为四区：

1. **设备区**：设备名、Host、发现、配对码、连接和 capability；
2. **交互区**：PTT、文字输入、输出偏好、停止播放、虚拟屏幕；
3. **当前轮次区**：client input、transcript、Input/Run、最终文字/播报/播放；
4. **诊断区**：按时间顺序显示控制消息摘要、frame 计数、耗时、错误和故障注入。

默认视图保持简洁；原始 envelope、frame header 和统计放在可折叠诊断区。H5 不展示完整 Runtime
Conversation、模型 reasoning、权限规则或工具参数。

## 九、能力预设

H5 至少提供以下互斥预设，并允许高级区逐项切换：

| 预设 | 输入 | 输出 | 用途 |
| --- | --- | --- | --- |
| Voice-only | PCM | PCM | 无屏音箱/机器人 |
| Screen voice | PCM + text | text + PCM | 带屏智能终端 |
| Keyboard screen | text | text | 带键盘终端 |
| Mixed | PCM + text | text + PCM | 动态切换验证 |

能力只能在断开或重新 hello 时改变；输出偏好可以在已协商能力范围内连接中切换。Host 返回 effective
capability 后，H5 必须禁用未协商操作，仍允许通过故障注入显式发送非法请求测试拒绝路径。

## 十、故障注入

Node 提供确定性、一次性或持续的故障开关：

- mDNS Host 消失/多个 Host；
- 错误配对码、过期 request、签名错误、旧 nonce；
- 重复 hello、重复 message ID、未知 major/minor；
- PCM sequence 重复/跳号、错误 length/reserved/kind、超长话语；
- 发送中断线、accepted 响应前断线、processing 断线、speaking 断线；
- 暂停读取模拟慢设备；
- 忽略 ping/pong；
- 重复 `playback_cancel`；
- client input 重放；
- 声称不具备的 output preference。

故障注入只修改 Node 对端行为，不能在 Host 内增加产品 fake bypass。

M7 的交互页把这些能力收在默认折叠的高级区。身份签名、协议主版本、重复控制 envelope、PCM 首帧
序号、输入后断线和忽略下一次 ping 是一次性故障；暂停读取、能力外输出偏好和重复
`playback_cancel` 是即时故障。错误配对码、配对窗口过期/取消、revoke、Host 消失/多 Host 和进程重启
仍由其自然产品动作触发，不再为同一事实增加模拟开关。

## 十一、自动化分层

### 11.1 Codec 单元测试

Rust 与 Node 对同一 fixtures 验证：

- JSON envelope 序列化、大小和 unknown field；
- 16-byte PCM header 大小端、frame 切分和非法输入；
- challenge/signature test vector；
- idempotency key 派生。

### 11.2 Host 组件测试

不启动浏览器，使用 scripted device peer：

- mDNS service metadata；
- pairing window、过期、错误次数、登记和 revoke；
- connection replace、heartbeat、capability；
- text input 到 fake Runtime；
- PCM 到 fake ASR、fake TTS 到 PCM；
- backpressure 和取消。

所有测试使用 `TempDir` Runtime Home、临时证书和临时 SQLite，不能打开用户真实数据。

### 11.3 Runtime 契约测试

使用 fake `ChannelOutputDispatcher`：

- Device input source、Queue、Conversation 和 restart；
- PC hosting 与终端来源优先级；
- 多次 speak 严格按调用顺序入队，且结束缺失时只补齐一次；
- Tool/Context/Goal continuation；
- dispatcher failure 与 Run settlement 独立。

### 11.4 Node/H5 测试

- Node codec/identity/reconnect 单元测试；
- 浏览器 AudioWorklet 的重采样和 PCM conversion fixture；
- Playwright 操作 H5：配对、文字、PTT、切偏好、播放、断线和故障注入；
- Playwright 只通过 H5 操作，不能测试代码直接调用 Node 内部 state mutation。

### 11.5 正式 Host 集成

启动真实 `ez-assistant-runtime`、Node simulator、Desktop 或 HTTP test client，Provider 使用可控 fake：

- 通过正式 Desktop Command 打开配对、输入码和设置托管；
- Node 通过 mDNS/WSS 配对并提交文字/PCM；
- fake ASR 返回确定 transcript，fake model 产生多 step/多次 `speak`，fake TTS 返回确定 PCM；
- 核对 Runtime SQLite/Conversation JSONL、Desktop UI 的完整显式输入/输出、Node 收到的 delivery 和无重复 Input；
- 进程重启验证设备注册、托管和历史来源恢复，不补播旧 output。

### 11.6 真实 Provider smoke

默认忽略、用户显式运行：

- 使用隔离 Runtime Home 和用户明确配置的 DashScope credential；
- 真实麦克风/PTT → ASR → Controller → speak → TTS → 浏览器播放；
- 保存开发 PCM 供人工比较；
- 输出中不得包含 credential、完整 Provider body 或私有 Runtime Token；
- 是否计费在执行前明确提示。

## 十二、版本验收场景

至少完成以下端到端场景：

1. 首次发现、显示配对码、Desktop 输入、配对完成和重连；
2. 错码、过期、取消、revoke 后旧设备无法重连；
3. Keyboard screen 文字输入进入主控并返回文字，同时 Desktop UI 可见输入和最终回复；
4. Voice-only PTT 输入由 Host ASR，transcript 与最终回复在 Desktop UI 可见，设备按顺序播放所有 `speak` 分段；
5. Screen voice 同时收到完整文字和压缩音频；
6. 同一轮三次 `speak` 调用按第一、第二、第三条顺序播放；
7. 审批等待不自行播放；工具 continuation 和 Goal continuation 中的真实 `speak` 追加到同一队列；
8. Desktop Input 在 A 托管、切换 B、解除托管三种路由；
9. A 终端主动输入时即使 PC 托管 B，附加 delivery 仍只到 A，Desktop UI 始终可见正文；
10. A 终端通过主控投递普通会话任务，代理报告返回 A，不被 PC 托管 B 截获；
11. 普通会话运行中才开启代理，产生的无明确渠道报告按报告结算时的 PC 托管投递；
12. accepted 前后断线、client input 重试和无重复 Run；
13. speaking 时新输入先停播但不修改已完成 Run；
14. TTS 失败、ASR 失败、慢客户端和坏帧均按边界收敛；
15. Host 重启后设备/托管/历史来源恢复，旧音频不补播；
16. Release 配置下未生成开发音频文件。

### 12.1 M7 验证映射

| 场景 | M7 确定性证据或明确人工步骤 |
| --- | --- |
| 1 | `npm run test:m7-e2e` 启动隔离正式 Host，经 mDNS/WSS 完成配对、重连和证书 pin。 |
| 2 | Desktop 设备设置测试覆盖候选与配对码校验，Runtime/Store 覆盖 revoke；人工时在隔离 Home 依次输入错码、关闭配对窗口后确认、重新配对并 revoke，旧 credential 必须认证失败。 |
| 3 | M3 正式文字 E2E 覆盖文字 Input/Run/Conversation；M7 正式 hello 额外核对 Keyboard screen 的精确能力，Desktop 来源弹窗测试核对展示。 |
| 4 | M7 正式 PCM/ASR/speak/TTS/下行闭环、Voice-only 正式 hello、录音/播放 Worklet fixtures 共同覆盖；真实麦克风链路已在 M5/M6 人工验证。 |
| 5 | M7 `text_and_audio` 正式 delivery 同时核对 `text_output` 和下行 PCM，并核对 Screen voice 正式 hello。 |
| 6 | Runtime `controller_speak_dispatches_every_segment_in_tool_call_order`、Host 有界播放队列测试和播放 Worklet marker 测试覆盖。 |
| 7 | Runtime 输出周期、缺失播报提醒、Goal/Tool continuation 测试覆盖；人工审批步骤为让工具进入等待，确认批准前没有新的 `playback_start`。 |
| 8 | Runtime hosting rename/revoke 与 Gateway 路由测试、Desktop 唯一托管入口测试覆盖 A/解除；切 B 在 Desktop E2E 中按同一入口操作并核对两台设备诊断。 |
| 9 | Runtime 来源优先测试覆盖“来源 A 高于 PC 托管 B”，Desktop Conversation 始终保留正文。 |
| 10 | `device_controller_delivery_report_returns_to_the_source_instead_of_pc_hosting` 覆盖。 |
| 11 | `enabling_proxy_during_an_active_run_reports_that_run_at_settlement` 覆盖结算时托管依据。 |
| 12 | M3/M5/M7 的同 client input 重试和断线重连复用同一 Input/Run；发送后断线为 H5 一次性故障。 |
| 13 | Host 新输入取消当前播放测试与正式 `playback_cancel` 路径覆盖；已完成 Conversation 不回滚。 |
| 14 | Speech Adapter/Service 失败测试、PCM sequence 正式故障、慢读/忽略 ping/Worklet 下溢与溢出 fixtures 覆盖。 |
| 15 | Store 重开测试覆盖设备/托管/来源持久事实；M8 使用隔离 Home 执行完整 Host 重启并人工确认旧音频不补播。 |
| 16 | 配置测试覆盖仅开发模式保存音频；M8 以 Release 配置启动隔离 Home，结束后检查临时音频目录不存在或为空。 |

## 十三、非验收捷径

以下路径可以用于局部调试，但不能作为版本完成证据：

- H5 直接调用 `/commands` 或订阅 `/events`；
- Node 直接把 H5 文字当作“ASR 已完成”走语音来源；
- 通过明文 WS，或仅设置 `rejectUnauthorized=false` 而未在任何设备应用消息发送前完成原始 DER
  证书指纹 pin 校验，以此完成正式成功场景；
- 绕过 Device Registry 手工指定 Device ID；
- Host 内置 fake Device/ASR/TTS 产品启动模式；
- 只检查 UI 状态，不核对正式 Conversation/Input/Run；
- 只跑真实 Provider happy path，不覆盖确定性 fake 和故障矩阵。
