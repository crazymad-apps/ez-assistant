# 原生网页媒体关闭回归

macOS 上运行真实 Tauri / WKWebView，直接复用产品的 BrowserResourceManager 与平台适配。
探针使用独立应用标识 `com.ez-assistant.browser-media-probe`，不装配、启动或连接 Assistant
Runtime，不读取用户会话或模型配置。夹具仅监听 loopback，播放低音量测试音，数据只在内存。

在仓库根目录启动夹具，记下打印出的随机端口：

```bash
python3 apps/desktop/tests/native/browser-media-fixture.py
```

另一个终端运行探针，将 `PORT` 替换为实际端口：

```bash
node apps/desktop/scripts/run-cargo.mjs run -p ez-assistant-desktop --example browser_media_close -- http://127.0.0.1:PORT
```

验证普通 audio 和 AudioContext 同时播放，截图、隐藏／恢复时不中断播放，截图进行中关闭标签
后不再发送页面心跳。成功输出 PASS；失败返回非零退出码，整体最多等待 30 秒。
关闭后的状态是夹具最后一次收到的记录，不能把其中旧的 `audio: true` 误读成仍在播放；断言
比较的是关闭后两次采样的时间戳，关闭前也核验过持续活动。

附加 `--raw-close` 可跳过媒体预清理，单独验证原生句柄的引用释放。修复前，经过截图后该模式
下心跳与两类播放时间持续增长；修复引用后应停止。它有助于防止只暂停声音却保留活动网页。

测试结束后用 Ctrl+C 停止夹具。此探针不替代发布版的人工窗口验收，也不代表 Windows／Linux
已通过原生媒体测试。

C03 M3 探针继续使用上述启动命令；媒体关闭验证后，还会打开个人、Alice、Bob 和再次登录的
Alice 四个页面，在同一 loopback origin 检查 Cookie/localStorage 隔离、本人持久存储恢复及旧
窗口 ID 的 owner 校验。身份只由 `browser-probe-owner.rs` 的测试夹具提供，实际 Profile 选择
和窗口管理复用产品代码；不开放页面 invoke，不访问产品 Host 或用户数据。macOS 14+ 的
持久 Profile 路径适用此用例；旧 macOS 的非持久降级及 Windows/Linux 需各平台另行验证。

# 原生输出跟随回归（C05）

启动 Vite 后，运行独立、非持久 WKWebView 夹具：

```bash
cd apps/desktop
npm run dev -- --host 127.0.0.1 --port 1436 --strictPort
# 仓库根目录另一个终端：
swift apps/desktop/tests/native/output-follow.swift http://127.0.0.1:1436/tests/native/output-follow.html
```

夹具直接引用产品 `useOutputFollow`，使用真实 WebKit 布局、ResizeObserver 和 scroll 事件，
检查增长跟随、上翻锚点、键盘回底、内外层独立意图；成功输出 PASS，30 秒超时失败。
不加载 Runtime 或用户数据。真实浏览器输入与正式 Host 组合验证见 `web-observation.spec.ts`。
