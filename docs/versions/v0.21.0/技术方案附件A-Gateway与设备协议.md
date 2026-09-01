# v0.21.0 技术方案附件 A：Gateway 与设备协议

## 一、文档定位

- 状态：已确认（2026-08-27）
- 上位方案：[`v0.21.0 技术方案`](技术方案.md)
- 约束范围：Runtime Host 内 Gateway、设备发现、配对、认证、WSS 控制消息、PCM frame、
  连接生命周期和安全边界

设备协议是 `runtime-host` 与智能终端之间的私有 Channel wire，不属于
`assistant-protocol`。本文冻结 protocol 1.0 的实现起点；后续真实固件可以复用 wire，
但不能反向改变 Runtime 的 Session、Input、Run 或 Conversation 契约。

## 二、Listener 与服务发现

### 2.1 两个监听边界

Runtime Host 同一进程持有两个相互独立的 listener：

| Listener | 绑定 | 安全 | 消费者 | 内容 |
| --- | --- | --- | --- | --- |
| Desktop HTTP | `127.0.0.1:0` | 进程 Bearer Token、Host/Origin | Tauri Desktop | Command、SSE、Upload |
| Device WSS | LAN address + 动态或配置端口 | TLS、设备挑战认证 | 已配对终端/配对中终端 | JSON control、PCM |

Device Listener 不复用 `runtime.json` 的地址或 Token，也不开放 Desktop Command 路由。
是否启用设备接入由 Host 配置和 Desktop 命令共同决定；关闭时停止 mDNS 宣告、拒绝新连接并结束
现有设备连接，不关闭 Runtime 或 Desktop listener。

两个 listener 由 `HostSupervisor` 分别跟踪，不共用一个不可区分的 server task。Desktop listener、
Runtime 或 Store 退出是进程级致命故障；Device Listener 或 mDNS 退出使设备子系统进入可观察降级状态，
不自动改写已配对设备或 PC 托管事实，Desktop 与 Runtime 仍继续可用。

### 2.2 mDNS

Host 宣告：

```text
service type: _ez-assistant._tcp.local.
instance:     <installation-id>._ez-assistant._tcp.local.
```

TXT 只包含非敏感发现信息：

```text
protocol_major=1
protocol_minor=0
path=/device
installation_id=<opaque-id>
certificate_fingerprint=<sha256>
pairing_available=true|false
```

不宣告 Runtime Home、Desktop Token、用户名称、Session ID、设备列表或 Provider 状态。mDNS 记录
只帮助定位和提供首次 TLS 指纹线索，不代表设备已经认证。

终端主动发现 Host 并建立出站 WSS。Host 不扫描终端，也不要求终端开放局域网端口。

## 三、安装身份与 TLS

### 3.1 Host 安装身份

第一次启用设备接入时，Host 在 Runtime Home 私有目录生成：

```text
device/
├── installation.json       # installation id、证书元数据
├── server-cert.pem         # 可公开证书
└── server-key.pem          # 0600 私钥
```

- 证书至少覆盖 mDNS 实例名和本机连接时实际使用的名称；
- 私钥创建与替换使用 `O_NOFOLLOW`、私有权限和同目录原子发布；
- 普通日志只记录证书指纹短摘要，不记录私钥或完整配对凭据；
- 证书损坏或私钥不匹配时设备 listener fail-closed，Desktop loopback 能力仍可诊断使用。

本版本不建设公网 CA、ACME、系统钥匙串或证书自动轮换。安装身份轮换会使设备重新配对，必须是
Desktop 的显式管理动作，不能在普通启动时静默发生。

### 3.2 首次连接与后续连接

- 未配对设备允许连接 WSS 配对子状态，但不能发送输入或 PCM；首次连接按 mDNS 指纹建立
  pairing-only TLS，先使用配对码驱动的 SPAKE2 PAKE 验证本次配对，再在同一 PAKE 会话内
  生成长期设备密钥，并把其公钥与 TLS 安装指纹绑定。
- 配对完成后，设备保存安装证书指纹；后续 TLS 必须严格 pin 该指纹，不接受系统任意证书。
  对自签名证书，客户端可以在自定义 TLS 连接路径中跳过默认 CA 链判定，但必须在发送任何设备
  应用消息前同步校验原始 DER 证书的 SHA-256 指纹；`rejectUnauthorized=false` 本身不能构成成功路径。
- 后续应用认证使用设备长期 Ed25519 私钥签署 Host nonce。Host 只保存公钥；单独复制 Device ID
  或旧控制消息不能重新认证。

该模型不把六位配对码当作长期 secret，也不把 Runtime Bearer Token下发设备。配对码不能用普通
hash/proof 替代 PAKE，否则截获 transcript 后可以离线枚举短数字码。

## 四、配对协议

### 4.1 设备准备

未配对 Node/终端首次进入配对时只生成：

- 随机临时 `pairing_request_id` 和 SPAKE2 临时状态；
- 6 位数字配对码，使用加密随机源生成；
- 配对会话 nonce；
- 默认展示名和自身能力声明。

只有 SPAKE2 双向验证通过后，设备才在内存中生成长期 Ed25519 key pair。在最终绑定和配对
提交完成前，它不是已配对凭据；任一步失败都销毁内存密钥和临时 PAKE 状态。H5 只显示
配对码和进度，不读取私钥。

### 4.2 时序

```text
Device                 Host/Gateway                 Desktop
  │ mDNS discover           │                          │
  │─ WSS pairing hello ────>│                          │
  │  request + PAKE msg     │                          │
  │<─ pairing_pending ──────│                          │
  │                         │─ pending snapshot ──────>│
  │                         │<─ confirm(code) ─────────│
  │<─ pairing_pake ──────────│                          │
  │  verify PAKE + gen key  │                          │
  │─ pairing_bind ─────────>│                          │
  │  public key + proof     │                          │
  │<─ pairing_bind_ack ─────│                          │
  │  device_id + host proof │                          │
  │─ pairing_commit ────────>│                          │
  │<─ pairing_complete ─────│                          │
  │─ authenticated hello ──>│                          │
```

详细规则：

1. `pairing_hello` 携带 request ID、默认名、capabilities、随机 device nonce 和 SPAKE2 device
   message；此时没有长期设备公钥或设备身份签名。配对码本身不在 wire 或普通日志中出现。
2. Host 只在 Desktop 已进入“添加设备”窗口时接受新的 pending request；否则返回
   `pairing_not_open`。
3. Host 内存保存 request、SPAKE2 对端消息、连接和过期时间；不写 SQLite。
4. Desktop 提交配对码后，Host 才创建自己的 SPAKE2 状态并返回 host message；双方计算共享 key，
   先双向确认覆盖 request ID、Host 证书指纹、双方 nonce 和 capabilities digest 的 PAKE transcript。
5. PAKE 双向确认通过后，设备在内存生成长期 Ed25519 key pair，再发送 `pairing_bind`。
   该消息携带设备公钥、公钥持有证明，以及使用 PAKE 派生 binding key 生成的 MAC；最终
   binding transcript 覆盖前述 PAKE transcript、设备公钥和协商能力。
6. Host 验证 MAC 和公钥持有证明后，在 pending 内存状态中预分配 Device ID，返回
   `pairing_bind_ack` 及 Host proof；此时 Runtime/Store 尚未产生 paired 记录。
7. 设备将私钥、Device ID 和 Host 证书指纹原子写入 pending credential，再发送同时由长期私钥
   签名并由 binding key 认证的 `pairing_commit`。
8. Host 验证 commit 后才调用 Runtime 执行设备登记事务；事务成功后返回 `pairing_complete`，
   设备将 pending credential 标记为 active，随后重新走 authenticated hello。
9. `pairing_complete` 丢失时，设备可使用 pending credential 尝试 authenticated hello；Host 已登记则
   认证成功并转为 active，未登记则清理 pending credential 后重新配对。
10. 配对窗口 5 分钟过期；同 request 最多 5 次失败尝试。超限、取消或 commit 失败后销毁
    PAKE/binding 状态；未完成登记时 Host 不留下 paired 记录，设备清理 pending credential。

配对码只用于把用户面对的终端与 Host pending request 对上并完成首次 PAKE；长期抗重放依赖设备
密钥、Host nonce 和已 pin 的 TLS 安装身份。Host 不允许客户端自选最终 Device ID。

### 4.3 Desktop 配对命令

公共命令只表达用户意图：

```text
SetDeviceAccessEnabled
OpenDevicePairingWindow
CloseDevicePairingWindow
ConfirmDevicePairing(pairing_request_id, pairing_code, display_name?)
RenameDevice(device_id, display_name)
RevokeDevice(device_id)
```

配对码使用类似现有 `SecretValue` 的脱敏请求类型，不导出到日志、事件或回显结果。pending 投影只
显示 request ID、终端自报的安全名称、能力摘要和剩余时间，不显示公钥、PAKE 消息或连接地址。

## 五、连接握手与能力协商

### 5.1 控制 envelope

所有文本 WebSocket message 是一个完整 UTF-8 JSON 对象：

```json
{
  "protocol_major": 1,
  "protocol_minor": 0,
  "message_id": "msg-opaque",
  "type": "hello",
  "payload": {}
}
```

约束：

- 单条控制消息上限 64 KiB；未知字段按具体消息的兼容策略处理，身份与媒体边界消息使用
  `deny_unknown_fields`；
- `message_id` 在当前连接中唯一，只用于诊断、响应关联和重放拒绝，不进入 Runtime Event；
- major 不一致立即返回 `unsupported_protocol` 并关闭；minor 采用双方较小值，只有显式声明的
  optional capability 才能启用；
- JSON parse、schema、字符串和集合大小在进入业务用例前完成校验。

### 5.2 已配对 hello

Host 连接后先发送：

```json
{
  "type": "auth_challenge",
  "payload": {
    "connection_id": "conn-opaque",
    "nonce": "base64url-random",
    "server_time_ms": 1787790000000
  }
}
```

设备返回 `hello`：

- Device ID；
- device nonce；
- 对 protocol version、connection ID、双方 nonce、Device ID 和 capabilities digest 的签名；
- firmware/simulator 版本；
- capabilities 与当前输出偏好。

Host 以 Registry 公钥验签，核对设备仍为 paired，再返回 `hello_ack`。认证前除 pairing 和 hello
以外的控制消息、二进制帧一律拒绝。

### 5.3 能力模型

能力按独立布尔/枚举协商，不使用“voice device”总开关：

```text
input.text
input.pcm16_16k_mono
output.text
output.pcm16_16k_mono
control.playback_cancel
display.status
display.transcript
```

Host effective capability 是设备声明、Host 当前 ASR/TTS 可用性和服务端策略的交集：

- ASR 不可用时不协商 PCM input，但 text input 仍可用；
- TTS 不可用时不协商 PCM output，但 text output 仍可用；
- `audio`/`text_and_audio` 偏好只有在相应输出能力存在时才接受；
- 偏好可以在连接内通过 `set_output_preference` 切换，Host 返回实际采用值。

## 六、控制消息集合

### 6.1 设备到 Host

| type | 关键字段 | 语义 |
| --- | --- | --- |
| `pairing_hello` / `pairing_confirm` | request、SPAKE2 message/key confirmation、能力 | 配对码验证 |
| `pairing_bind` / `pairing_commit` | 长期公钥、持有证明、binding MAC/commit proof | PAKE 通过后的长期身份绑定 |
| `hello` | Device ID、nonce、签名、能力 | 已配对认证 |
| `text_input` | `client_input_id`、text、output preference | 正式文字输入 |
| `listen_start` | `client_input_id`、stream ID、格式、preference | 声明上行话语 |
| `listen_stop` | stream ID、last sequence | 正常结束话语 |
| `listen_cancel` | stream ID | 丢弃未提交话语 |
| `set_output_preference` | preference | 更新当前默认输出偏好 |
| `playback_cancel` | output ID/stream ID | 只取消播放 |
| `ping`/`pong` | nonce、时间 | 应用 RTT 诊断 |

### 6.2 Host 到设备

| type | 关键字段 | 语义 |
| --- | --- | --- |
| `auth_challenge` / `hello_ack` | nonce、connection、effective capabilities | 认证协商 |
| `pairing_pending` / `pairing_pake` | request、SPAKE2 host message/key confirmation | 配对码验证进度 |
| `pairing_bind_ack` / `pairing_complete` | Device ID、Host binding proof、commit result | 长期身份绑定与登记结果 |
| `input_segment_accepted` | client input、stream ID | Host 已接管该段 PCM，终端可释放分段缓存 |
| `input_accepted` | client input、Input ID、Run ID、queue state | Runtime 已可靠接受 |
| `transcript` | client input、text | ASR 成功结果 |
| `state_changed` | interaction、state、reason? | 终端有限状态 |
| `text_output` | output ID、Run ID、text | 完整 Assistant 文本 |
| `playback_start` | output/stream、格式、最终播报文本、optional samples | 下行 PCM 开始 |
| `playback_end` | output/stream、reason | 正常/失败/取消结束 |
| `error` | code、correlation、recoverable | 稳定错误 |
| `connection_replaced` | new connection hint | 旧连接退出 |

## 七、PCM 二进制帧

### 7.1 音频格式

首版只接受与 `agent-robot` 已回证基线一致的格式：

```text
signed PCM16 little-endian
16,000 Hz
mono
20 ms / frame
320 samples / frame
640-byte payload
```

不在 Device Adapter 内做任意格式自动猜测。未来增加 Opus 或其他采样率必须以新 capability 和
明确 codec 扩展，不能静默改变 protocol 1.0 的 PCM 语义。

### 7.2 16-byte header

| offset | bytes | field | encoding |
| ---: | ---: | --- | --- |
| 0 | 1 | header version | `1` |
| 1 | 1 | media kind | `1=uplink_pcm`, `2=downlink_pcm` |
| 2 | 2 | flags | network byte order；v1 为 `0` |
| 4 | 4 | audio stream ID | network byte order |
| 8 | 4 | sequence | network byte order，从 `0` 连续递增 |
| 12 | 2 | payload length | network byte order，v1 为 `640` |
| 14 | 2 | reserved | 必须为 `0` |

payload 紧随 header，样本本身保持 little-endian。正常结束由 `listen_stop`/`playback_end` 表达，
零长帧或 WebSocket close 都不能伪装成正常结束。

### 7.3 校验和背压

接收端必须拒绝：

- 未声明或已结束 stream 的帧；
- header version、kind、flags、reserved 或 payload length 非法；
- 序号重复、回退、跳号；首版局域网 WebSocket 不做丢包补洞；
- WebSocket message 重组后不是恰好 `16 + payload_length`；
- 超过单话语字节、时长或并发上限；
- 下行帧出现在设备到 Host 的方向，或反之。

每连接控制消息与媒体分别使用有界队列。媒体队列满时取消该 stream 并返回
`media_backpressure`，不无限缓存，也不阻塞 Runtime 执行。

## 八、输入幂等

终端为每次文字输入或 PTT 分段生成稳定 `client_input_id`。语音分段在收到
`input_segment_accepted` 或明确不可重试失败前保持不变；多个连续分段合并后，以第一段 ID 作为
逻辑输入 ID。Host 派生：

```text
device-v1:<device-id>:<client-input-id>
```

若长度超过 `IdempotencyKey` 上限则使用带版本前缀的 SHA-256 摘要。相同设备和 client input 重试
必须落到相同 Runtime key；不同设备即使 client input 相同也不能冲突。

Host 完整接管 PCM 后发送 `input_segment_accepted`；该确认不创建 Runtime Input。每次 `listen_stop`
重置 2 秒提交窗口，窗口内新 `listen_start` 立即暂停提交，分段 transcript 按录音顺序以换行合并。
ASR 失败、空话语和用户 `listen_cancel` 不调用 Runtime。Runtime 接受整轮后才发送一次
`input_accepted`；响应丢失时相同逻辑 key 返回首次 Input/Run。

## 九、心跳、替换与重连

- WebSocket ping 每 15 秒；10 秒无 pong 结束连接；应用 ping/pong 只做 RTT 和 UI 诊断；
- 重连退避为 1、2、4、8、15 秒并加抖动，连续稳定 60 秒后重置；
- 新连接完成认证和 hello 后原子替换旧连接，之后才发送 `connection_replaced`；
- listening 断线：Host 丢弃未完成话语；Node 可用相同 client input 和完整音频重新发送；
- 已发送 `input_segment_accepted` 的分段聚合与 ASR 任务按 Device ID 保留在 Host 进程内，不随 WSS
  替换销毁；Host 进程重启仍会放弃尚未形成 Runtime Input 的聚合轮次；
- processing 断线：Run 继续，重连不重复提交，也不补播已完成回复；
- speaking 断线：停止发送并取消本连接媒体任务，重连不从头播放；
- `playback_cancel` 只结束 output stream，不调用 `CancelRun`。

## 十、安全与限额

首版至少固定以下上限，具体数值在实现 spike 后写入常量和 capability：

- 单安装 pending pairing 数；
- 单设备活动连接数为 1；
- 单连接同时上行话语数为 1、下行播放数为 1；
- 控制消息 64 KiB；PCM payload 640 bytes；
- 单话语最长时长、总字节和 ASR 超时；
- 文本输入与 `speak` 文本的字符/字节上限；
- 签名 nonce 有效时间与已使用 message ID 窗口。

普通日志不得记录配对码、设备公钥全文、签名、transcript、Assistant 正文或音频。开发诊断可以记录
Device ID 短摘要、connection/output/stream ID、帧数、字节数、耗时和稳定错误码。

## 十一、协议 fixtures

实现时建立一份 Rust 与 Node 共同读取的 fixture 目录，至少包含：

- 每种控制 envelope 的合法最小样例；
- unknown major、超长字段、重复 message ID 等非法样例；
- 上/下行第 0 帧和连续多帧的二进制 golden；
- 大小端、reserved、payload length、sequence 错误样例；
- 签名 challenge 的确定性测试向量；
- SPAKE2 双方消息、shared key confirmation 和错误配对码测试向量；
- 长期公钥持有证明、binding MAC、commit proof 和最终 transcript 测试向量。

fixture 是协议测试资产，不从 Rust 类型生成后手工修改两份。Rust codec 与 Node codec 都必须对同一
文件做正反向测试，避免“两个实现分别自测都通过但互不兼容”。
