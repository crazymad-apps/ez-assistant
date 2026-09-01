# Device Simulator

Node/H5 智能终端模拟器。Node 通过正式 mDNS/WSS 设备协议与 Runtime Host 通信，浏览器页面
用于选择 Host、配对、终端形态、文字输入、麦克风录音、播放和状态监控。H5 录音由
AudioWorklet 采集并转换为固定 PCM16/16 kHz/mono/20 ms frame，经私有二进制桥交给 Node；
ASR 始终在 Host 内完成。

需要 Node.js 24 LTS：

```bash
npm ci
npm run typecheck
npm test
npm run lint
npm run build
npm run test:e2e
npm start -- --home /absolute/private/simulator-home
```

进程启动后会输出仅绑定 `127.0.0.1` 的 H5 地址。先通过 Desktop/Host Command 启用设备接入并
打开配对窗口，再在页面选择发现的 Host。长期设备凭据只写入 `--home`，开发和测试应使用
专用临时目录，不要指向 Runtime Home。

`npm run dev` 使用 TypeScript 直接启动 Node，但 H5 仍从 `dist/web` 读取，因此首次使用前需要先执行
`npm run build`。

真人语音测试需允许浏览器访问麦克风。点击“开始录音”，说话后点击“停止并发送”；录音最长 60 秒，
停止后才作为一个完整话语上传。“发送测试语音”仍保留为不依赖麦克风的协议测试音。

“纯语音、带屏语音、键盘屏幕、混合终端”会在下一次正式 WSS hello 中声明不同能力，因此只能在
断开时切换。折叠的故障注入区提供一次性签名、版本、重复消息、PCM 序号、断线、慢读、心跳、
能力越界和重复停止播放故障；它们只改变 Node 对正式设备协议的行为，不会在 Host 开启测试后门。
