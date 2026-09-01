# device-simulator 模块约束

## 模块定位

`tools/device-simulator` 是开发者显式启动的 Node 智能终端模拟器。Node 进程通过正式
mDNS/WSS 设备协议连接 Runtime Host；H5 页面只通过 loopback 私有桥操作和观察 Node。
它不是正式终端、Desktop 客户端、Runtime Client、产品 sidecar 或 daemon。

修改前必须阅读当前版本功能设计、技术方案附件 A/D 与开发计划；涉及 H5 时还需遵守
[`前端编程规范`](../specs/前端编程规范.md)。

## 核心约束

- H5 不得直连 Runtime `/commands`、`/events`、数据库或 Device WSS。所有验收行为必须经过
  Node 的正式设备 codec 和连接状态机。
- Node 不装配 Agent、ASR、TTS 或 Runtime Client。H5 文字输入是设备 `input_text`，不是伪造
  的 ASR transcript。
- 模拟器私有 DTO、状态和诊断事件不得进入 `assistant-protocol`；正式设备 wire 仍由
  Runtime Host 私有 Gateway 契约定义。
- 模拟器只写入显式 `--home` 或测试临时目录，不读取 Runtime Home。长期 Ed25519 密钥只能在
  SPAKE2 双向确认完成后生成；私钥不得进入 H5、日志、错误或 fixture。
- Host 自签名证书允许在 TLS 握手阶段使用自定义验证路径，但在发送任何设备应用消息前必须对
  原始 DER 证书执行 SHA-256 pin 校验。`rejectUnauthorized: false` 不能单独形成成功路径。
- H5 server 只绑定 `127.0.0.1`，控制 WebSocket 校验页面 token 与 Origin；诊断使用固定容量
  ring buffer。
- Node 版本基线为 24；源码、测试与构建不得依赖用户全局安装包或隐式网络服务。
- 故障注入只能改变模拟器对端行为，不得要求 Host 添加 fake bypass。
- M3 的文字输入、同一 `client_input_id` 重试、`input_accepted` 和 `text_output` 都由 Node 正式 WSS
  codec 完成；H5 只提交普通 `submit_text`/`retry_text` 操作并展示 Node 快照，不直接读取 Runtime。
- M5 的测试语音可由 Node 生成确定性样本，也可由 H5 经用户授权采集麦克风并转换为
  PCM16/16 kHz/mono/20 ms frame；H5 只通过 loopback 私有二进制桥把完整话语交给 Node，不执行 ASR、
  不直连 Host。Node 仍经正式 `listen_start`、二进制帧和 `listen_stop` 上传，同连接和重连后的
  `retry_pcm` 保留相同 client identity。
- M6 的 Node 对端必须声明并校验 PCM 输出与播放取消能力，严格处理 `playback_start`、direction kind
  为 `2` 的固定帧、真实 sample count 和 `playback_end`。Node 通过私有 loopback bridge 把已经校验并按
  sample count 截取的 PCM 交给 H5；浏览器仅在用户显式启用后使用 Web Audio 播放，并可经 Node 发送正式
  `playback_cancel`。为避免“浏览器已允许发声”与“设备仍请求纯文字”产生歧义，H5 启用播报时若当前偏好
  为纯文字，必须同步切换为 `text_and_audio`；已经冻结偏好的历史输入不追溯补播。Node/H5 均不执行 TTS，
  H5 把 16 kHz PCM 做跨传输帧连续重采样后送入单一长期 AudioWorklet 流；Worklet 在渲染线程执行
  120 ms 首缓冲和 80 ms 下溢恢复，相邻 `speak` 保留 160 ms 气口，完成状态等待对应 marker 真正排空。
  本地容量或处理器故障必须明确显示，但不得自动发送 `playback_cancel` 清空 Host 的其他待播片段。
- M7 的录音输入必须由长期 AudioWorkletNode 从渲染线程采集 mono Float32；主线程只接收转移后的
  样本、执行确定性 16 kHz/PCM16 转换并在停止时一次性交给 Node。不得退回已废弃的
  `ScriptProcessorNode`，权限拒绝、Worklet 错误和 60 秒上限必须释放 MediaStream 与 AudioContext。
- 四类终端形态必须映射为一组明确的 declared capabilities 和默认输出偏好；只允许在断开状态切换，
  Host 返回的 effective capabilities 继续决定可用操作。H5 不逐项拼装一套未经过握手的本地能力状态。
- 故障注入通过 loopback 私有控制面设置一次性或即时的 Node 行为，正式错误仍必须由 Device WSS
  触发。签名、协议版本、重复 message ID、PCM sequence、发送后断线、慢读、忽略 ping、能力外偏好和
  重复 playback cancel 均不得改写 Host 配置或 Runtime Store。

## 分层

- `src/node/`：身份、发现、TLS pin、WSS、协议状态机、本地 H5 bridge。
- `src/web/`：设备状态监控和操作页，不保存权威设备状态。
- `tests/`：codec、密码学共享向量、身份原子写入和状态机测试。
- `docs/resources/device-protocol-v1/fixtures/`：Rust 与 Node 共用的非敏感协议 fixture。

## 验证

```bash
cd tools/device-simulator
npm ci
npm run typecheck
npm test
npm run build
npm run test:m5-e2e
npm run test:m6-e2e
npm run test:e2e
```

正式里程碑验收还必须以临时 Runtime Home 启动真实 `ez-assistant-runtime`，通过 Host Command
打开配对窗口并由 Node 完成 mDNS/WSS 配对；只运行 H5 或 Node 单元测试不能替代端到端证据。
