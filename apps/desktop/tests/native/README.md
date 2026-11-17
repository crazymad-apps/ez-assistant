# F03 正式 Tauri CSP 承载回归

根 Cargo 的 Wry 覆盖是 Windows 原生隔离的必要部分，来源及维护方式见
[受控依赖说明](../../../../vendor/README.md)。探针同时要求主 frame 获得真实 IPC 回执，
子 frame 不运行 main-only 初始化、不获得 Tauri 对象，而显式 all-frame 初始化仍执行。

先构建当前 Host 和原生探针，再运行隔离验证：

```bash
cd apps/desktop
npm run build:host
npm run cargo -- build -p ez-assistant-desktop --example mcp_apps_probe --features tauri/custom-protocol
npm run test:native:mcp-apps
```

Windows 应先经正式 `npm run tauri -- build --no-bundle` 归一化 Tauri 管理的平台 feature，
再构建原生 example。直接使用在 macOS 上经 CLI 处理过的清单执行 Windows Cargo 构建，
会被 Tauri 的 feature／配置一致性检查拒绝；不要为此手删平台依赖或放宽 CSP。

脚本新建 `.runtime-test/` 场景，经正式 Host 资源接口读取通用 MCP HTML，再通过产品
`McpAppView` 与 Tauri 正式资产协议加载。探针保持产品 `security.csp`，使用唯一应用标识与
非持久 WebView，只开放窄测试回执，不装配第二个 Runtime，不访问用户 Home。检查握手、
父 DOM／Cookie 内容／原生命令桥隔离与卸载；WebKit 的 Cookie getter 可能返回空字符串，
按内容是否泄漏判断，不能仅以 getter 是否抛错推断隔离。

M1 同时检查协商后的快照通知、请求、成功回执及卸载退役；此原生场景使用通用测试保存回调，
不声称已经写入真实快照。真实发布、Store CAS 和刷新恢复由 `web-message-cards.spec.ts` 与
Runtime／Host 存储测试覆盖；原生场景仍要求 SQLite 全表数量不变。

场景保留供核查，原生探针有界退出，Host 停止后核对 SQLite 全表精确数量。脚本只生成测试
资源，不复制登录凭据到页面；不得将场景目录改为用户 Home。可用显式
`EZ_ASSISTANT_E2E_HOST_BINARY`／`EZ_ASSISTANT_MCP_APPS_PROBE_BINARY` 指定已构建的匹配二进制。
M2 追加两份实际隔离页面的关联定位／回传验证，使用通用关联回调，确保不同 Window 的消息不会混用。
正式资源栏、目录、草稿和来源授权由 Host Web 端到端用例验证。
本探针不代替完整产品窗口、平台菜单或 Feature 联合验收；各平台结论分别记录。

# F03 MCP Apps 承载前置检查

对显式启动于新建 `.runtime-test/` 场景的隔离 Host 运行：

```bash
swift apps/desktop/tests/native/mcp-apps-csp.swift http://127.0.0.1:PORT
```

该探针使用非持久 WKWebView，只打开匿名页面，验证正式 Host 响应 CSP 拒绝 `data:` 代理。
它不登录、不提交业务命令，不加载宽松替代网页；成功表示原承载路径被否决，不能视为新方案
或 M0 通过。Host 自身的启动初始化仍写入该新建测试 Home，不能传入用户运行环境。
它不替代 Tauri 正式配置或 Windows WebView2 的检查。

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

# 原生主题启动与窗口恢复（F04 M0）

先在 Desktop 工程运行 Vite：

```bash
npm run dev -- --host 127.0.0.1 --port 1437 --strictPort
```

仓库根目录另开终端，新建一次性场景后连续运行两轮：

```bash
probe_dir=$(mktemp -d "$PWD/.runtime-test/f04-native-XXXXXX")
node apps/desktop/scripts/run-cargo.mjs run -p ez-assistant-desktop --example appearance_probe -- "$probe_dir"
node apps/desktop/scripts/run-cargo.mjs run -p ez-assistant-desktop --example appearance_probe -- "$probe_dir" second
```

探针直接复用产品偏好模块、主窗口构造、原生应用命令及前端 AppearanceStore；沿用平台窗口
配置和窗口状态插件。第一轮从 Monokai 启动，检查四套投影并保存 Solarized Dark；第二轮
确认该选择和窗口尺寸／位置恢复。每轮检查原生明暗、WebView 媒体查询、隐藏／唤起与保存，
最多等待 45 秒，失败返回非零退出码。

不装配 Host、不操作数据库。偏好和窗口状态位于所传的新场景目录；应用身份带场景后缀，
WebView 使用非持久模式。窗口状态插件使用场景中的绝对文件名；插件会建立独立应用配置空目录，
退出时仅尝试移除该空目录。不得传入用户配置或复用无关场景。

macOS 的主体加载前底色受 Sidebar 材质影响，不保证与预设 RGB 完全一致。此夹具不代表正式
Host 页面、所有系统首帧、减少透明度、更新重开及其他平台已经验收；完整界面和渲染器由后续
里程碑验证。结束后停止这次 Vite 即可。


# v0.33.1 F04 侧栏排序

先完成 `npm run build:host`，再在 Desktop 工程运行 `node scripts/verify-sidebar-native.mjs`。
探针为本次场景新建 `.runtime-test/f04-m2-native-*`，通过正式命令创建测试会话，
以非持久 WKWebView 加载内嵌页面；原生鼠标事件验证短按、拖动，Esc 和第二个真实窗口验证取消。
完成后核对 Host 快照与只读 SQLite 计数／完整性。保留测试现场，不读取用户 Runtime Home。

本探针只证明 macOS WKWebView 输入与正式 Host 组合，不代替完整 Tauri 客户端、Windows
原生或真实跨设备验收。测试源码与脚本不用于产品启动。
