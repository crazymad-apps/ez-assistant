# 基于 CDP 协议实现 Computer Use 探索

- 记录日期：260821
- 状态：探索输入（后续版本候选，尚未启动正式版本设计）
- 来源：CDP 协议规范与真实 Chrome 连通性验证；Anthropic computer use 参考实现源码
  调研；Rust CDP 生态 crates.io 调研
- 目标版本：待定（延续 `v0.17.0` 工具图片回传基础，作为未来浏览器/屏幕控制能力候选）

## 一、结论先行

基于 CDP（Chrome DevTools Protocol）在 ez-assistant 中实现 computer use 是**可行且与
现有架构契合**的。关键依据：

1. CDP 原生覆盖 computer use 的全部动作面：截图（`Page.captureScreenshot`）、鼠标键盘
   （`Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` / `Input.insertText`）、导航
   （`Page.navigate`）、JS 执行（`Runtime.evaluate`）、DOM 坐标定位
   （`DOM.getBoxModel` / `DOM.getContentQuads`）和页面语义结构
   （`Accessibility.getFullAXTree`）。这些已在本地 Chrome 151 headless 实例上逐条实测通过。
2. 本仓库 `v0.17.0` 已经打通"工具生成图片 → 稳定 Image Part → 三条模型投影"链路，
   computer use 的截图正是该链路的直接消费者；`agent-tools` 的能力契约 + 标准工具壳 +
   装配注入模式（如 `read_image` / `ImageMaterializer`）可以原样复刻给浏览器能力。
3. CDP 是"浏览器内事件注入 + 页面状态读取"，不是屏幕级模拟：它不需要 xdotool 之类的
   系统级工具、不需要虚拟显示器，也不需要"当前用户桌面"权限，安全边界比 Anthropic
   的 Docker + xdotool 方案清晰得多；它天然与"浏览器标签"这一产品形态对应。
4. 局限也要明确：CDP 只能控制 Chromium/Chrome（含 Electron），**不能控制整个桌面**。
   若产品目标是"任意系统应用"（真正意义上的 computer），CDP 方案只覆盖浏览器子集，
   必须与 macOS Accessibility（AXUIElement）或系统级输入模拟分开评估。对多数 Agent
   场景（浏览、填表、自动化网页操作）浏览器子集已足够，且更安全可控。

## 二、CDP 协议概览与 computer use 映射

CDP 通过两条入口访问：

- HTTP 端点：`http://127.0.0.1:<port>/json/version`（浏览器信息与 browser WebSocket）、
  `/json/list`（当前 target 列表，每个 target 有 `webSocketDebuggerUrl`）；
- WebSocket 端点：`ws://.../devtools/page/<id>`（页面级调试）与
  `ws://.../devtools/browser/<id>`（浏览器级：创建/关闭 target、浏览器窗口、下载行为等）。

协议主体是 JSON-RPC 风格：客户端发 `{"id", "method", "params"}`，服务端回
`{"id", "result"}` 或 `{"id", "error"}`；`method` 形如 `Domain.command`，事件以
`{"method", "params"}` 推送。官方的 browser_protocol + js_protocol JSON 定义可直接生成
类型绑定（chromiumoxide 就是这样做的）。

### 2.1 computer use 动作面 → CDP domain 映射

| Computer use 动作 | CDP 命令 | 说明 |
| --- | --- | --- |
| 截图（screenshot） | `Page.captureScreenshot` | 返回 base64 PNG/JPEG，可 `clip` 局部、`captureBeyondViewport` 全页 |
| 鼠标移动/点击/拖拽 | `Input.dispatchMouseEvent` | `mousePressed/mouseReleased/mouseMoved/mouseWheel`，带 x/y、button、clickCount |
| 键盘输入 | `Input.dispatchKeyEvent` + `Input.insertText` | keyDown/rawKeyDown/char + text；`insertText` 直接注入文本（推荐用于中文等 IME 文本） |
| 滚动 | `Input.dispatchMouseEvent` type=mouseWheel 或 `DOM.scrollIntoViewIfNeeded` | 后者按元素定位更稳 |
| 打开页面/导航 | `Page.navigate` / `Target.createTarget` | 页面级导航或新建 tab |
| 读取页面内容 | `Runtime.evaluate` | 任意 JS 求值，取 DOM 文本、表单值、状态 |
| 元素坐标定位 | `DOM.getDocument` + `DOM.querySelector` + `DOM.getBoxModel` | 得到元素 border box 四角像素坐标，供点击/框选 |
| 点击命中元素 | `DOM.getNodeForLocation` | 由屏幕坐标反查节点（与 `getBoxModel` 互逆） |
| 页面语义结构 | `Accessibility.getFullAXTree` | 无障碍树：role/name/value/坐标，是"模型理解页面"的推荐输入（比裸 DOM 省 token 且语义化） |
| 等待/观察 | `Page.startScreencast` + 事件 | 帧流式推送，可用于长时间观察（非首版必需） |
| 弹窗处理 | `Page.handleJavaScriptDialog` | alert/confirm/prompt 应答 |
| 文件下载/上传 | `Browser.setDownloadBehavior` / `DOM.setFileInputFiles` | 下载目录与 file input 注入 |
| 多标签/多窗口 | `Target` / `Browser.getWindowForTarget` / `setWindowBounds` | 窗口控制（首版可不做） |
| 视口/设备模拟 | `Emulation.setDeviceMetricsOverride` | 移动端视口（可选） |

### 2.2 实测验证（本机 Chrome 151.0.7922.170 headless）

用极简 Python WebSocket 客户端（标准库 + `websockets` 15.0.1）在
`--headless=new --remote-debugging-port=9222` 的本地 Chrome 上完整走通：

1. `Page.navigate` 到 data: URL 演示页（含 `<button onclick>` 和 `<input>`）；
2. `Runtime.evaluate` 读取 `document.title` 与节点数；
3. `DOM.getDocument` + `DOM.querySelector("#btn")` + `DOM.getBoxModel` 取到按钮
   border 坐标 `[8, 76.4, 75.1, 76.4, 75.1, 97.4, 8, 97.4]`，计算中心点 `(41, 86)`；
4. `Input.dispatchMouseEvent`（mousePressed + mouseReleased, button=left）模拟点击，
   随后 `Runtime.evaluate` 读 `document.title` 变为 `CLICKED!` —— **真实点击生效**；
5. `Input.insertText` 注入 `hello cdp`，读输入框 value 确认；
6. `Page.captureScreenshot` 得到 8507 字节 PNG，图片内容经识图确认与页面一致
   （标题、按钮、已聚焦输入框、已输入文本）；
7. `Accessibility.getFullAXTree` 返回 17 个节点，可提取 heading/button/textbox 的
   role 与 name（`CDP Probe` / `Click me` / `type here`）。

验证脚本在 `/tmp/cdp-probe/probe.py`（一次性探索用，非产品代码，未入库）。
结论：CDP 对"截图 → 理解 → 定位 → 操作 → 再截图"的闭环有完整、稳定的原生支持。

## 三、参考实现调研

### 3.1 Anthropic computer use（系统级，非 CDP）

Anthropic 的 `computer-use-demo`（anthropics/anthropic-quickstarts）是当前最被广泛
引用的 computer use 参考。它走的是**系统级输入模拟**路线，与 CDP 不同：

- 工具形态：单一 `computer` 工具，`action` 判别参数：
  - 20241022：`key`、`type`、`mouse_move`、`left_click`、`left_click_drag`、
    `right_click`、`middle_click`、`double_click`、`screenshot`、`cursor_position`；
  - 20250124 增加：`left_mouse_down`、`left_mouse_up`、`scroll`、`hold_key`、
    `wait`、`triple_click`；
  - 20251124 增加：`zoom`。
- 实现：Docker 容器内 xdotool（鼠标/键盘）+ gnome-screenshot/scrot（截图）+
  ImageMagick `convert` 缩放。每次动作后默认延时 2 秒再截图回传。
- 坐标缩放：`scale_coordinates` 把屏幕缩放限制到 XGA/WXGA/FWXGA（1024x768 /
  1280x800 / 1366x768），模型 API 坐标与真实屏幕坐标双向换算 —— 这是"截图喂给模型"
  时降低视觉推理难度的关键工程细节。
- 局限：需要真实/虚拟显示器、系统级工具链、容器权限；无法精确读取页面语义结构
  （只能靠截图），动作粒度是屏幕像素而非 DOM 元素。

对比：CDP 方案不需要 xdotool/显示器，点击可直接基于 `getBoxModel` 的像素坐标（与
Anthropic 的坐标语义兼容），同时还能拿到 AX 树等结构化信号。**模型可见的动作集可以
复用 Anthropic 的 action 枚举设计**，这有利于未来模型（如已训练过 computer use 动作
的模型）直接复用。

### 3.2 OpenAI computer use（浏览器平台服务）

OpenAI 的 computer use 是"托管浏览器 + CDP"路线：browserless（第三方浏览器云）提供
CDP 端点，OpenAI 服务通过 CDP 操作浏览器，动作集类似 Anthropic（`mouse_click`、
`mouse_move`、`type_text`、`scroll`、`wait`、`screenshot`、`get_dom` 等），并通过
`computer_20250128` 等工具类型声明给模型。它证明了 **CDP 作为 computer use 后端完全
成立**；但依赖托管浏览器与远端 API，本地优先的 ez-assistant 更适合"本地 Chrome +
本地 CDP"。

### 3.3 Playwright MCP / 浏览器 MCP 生态

- `microsoft/playwright-mcp`（约 36k stars）：MCP server 形态的浏览器自动化，把
  Playwright 能力封装为 `browser_navigate`、`browser_click`、`browser_type`、
  `browser_screenshot` 等 MCP 工具。它是"浏览器操作工具"而非"computer use 屏幕
  语义"，但工具命名与参数设计可参考。
- crates.io 上也有 `native-devtools-mcp` 等把 CDP + 截图/OCR/模板匹配做成 computer
  use MCP server 的尝试，说明"CDP 支撑 computer use"已是成熟共识。
- 对 ez-assistant 的意义：本仓库正在讨论 MCP 机制（见
  `260821-MCP机制讨论与Agent实现调研.md`），浏览器能力既可以做成内置一等工具，也
  可以未来通过 MCP 接入。首版建议内置，理由见第五节。

### 3.4 Rust CDP 生态

| crate | 版本 | 下载量 | 说明 |
| --- | --- | --- | --- |
| `chromiumoxide` | 0.9.1 | ~350 万 | 最成熟的高层 CDP 库：`Browser::launch`、`new_page`、`find_element().click()/type_str()`、`Page`/`cdp` 全量类型；含浏览器 fetcher；能启动/连接 headless Chrome。编译较重（生成代码约 6 万行） |
| `chrome-devtools-rs` | 0.0.0-alpha.3 | ~8.3 万 | 低层 CDP 客户端 |
| `cdpkit` | 0.7.1 | ~900 | 类型安全 CDP 客户端 |
| `tokio-tungstenite` | 0.30.0 | ~2.5 亿 | WebSocket 传输层，若自研 CDP 传输可用 |

选型倾向：**chromiumoxide** 覆盖启动、连接、Page/Element、CDP 命令与事件，最省力；
但它也引入了较重的生成代码和自身的事件循环模型（需要 `handler.next()` 循环消费），
与 `agent-tools` 的"能力契约 + 一次性 executor"结合时需要包一层薄 Adapter。备选方案
是只用 `tokio-tungstenite` + `chromiumoxide_cdp`（纯 CDP 类型，无浏览器启动逻辑）自研
传输层 —— 更贴合本项目"自己掌控协议与生命周期"的风格，但工作量明显更大。

## 四、与 ez-assistant 架构的对接方案

### 4.1 复用既有模式：能力契约 + 标准工具壳 + 装配注入

完全沿 `read_image` / `ImageMaterializer` / Shell 的既有分层，新增一个"浏览器能力"：

```text
agent-tools（能力契约层，无副作用）
  capability::browser::BrowserTool trait
    - screenshot / navigate / eval / click / type / key / scroll / get_ax / ...
    - 请求/结果/错误类型；不含任何 CDP 或 Chrome 实现细节
  standard::browser::BrowserComputerTool（标准工具壳）
    - 固定模型可见名称与 Schema（复用 Anthropic action 枚举风格）
    - resolve 只做参数校验与授权事实生成
    - execute 委托装配注入的 BrowserTool 实现

agent-tools-local（或新 adapter crate，本地实现层）
  cdp::ChromiumBrowserAdapter
    - chromiumoxide 或自研 ws 传输；持有浏览器连接池/单浏览器句柄
    - 实现 BrowserTool 的每个动作；截图走 ToolImageReference 物化
    - 负责 Chrome 生命周期：launch、reconnect、shutdown、超时与取消

Runtime / Host（装配层）
  - 注册标准工具壳；注入权限、审批、审计；按 Session/配置装配浏览器能力
```

### 4.2 工具形态建议（首版候选）

方案 A（推荐）：**单一 `browser` 工具 + action 判别**（对齐 Anthropic）：

```text
browser(action: screenshot|navigate|click|type|key|scroll|eval|get_ax, ...)
```

- 优点：与主流 computer use 模型训练分布一致；模型只需记一个工具的 Schema；
  每个动作后由实现决定是否自动截图回传。
- 缺点：单工具参数 Schema 较大（判别分支多）。

方案 B：按动作拆多个工具（`browser_screenshot`、`browser_click`、`browser_type`…）。

- 优点：Schema 小、可单独做权限控制与审批。
- 缺点：工具数量多；与 Anthropic 动作语义不一致，模型需重新学习。

折中：首版用方案 A 的 `browser` 工具承载全部动作，但把"截图自动回传"做成动作内
选项（`take_screenshot: bool = true`），与 Anthropic `screenshot` 动作对齐。

### 4.3 截图回传直接复用 v0.17.0 链路

- `Page.captureScreenshot` 得到 base64 PNG → 解码为字节 → 走既有
  `tool-images/` 物化（SHA-256 去重 + MIME 校验）→ 返回 `ToolResultPart::Image`。
- 三条投影（原生视觉主模型 / 内部图片输入 / 辅助识图）无需任何新工作，
  v0.17.0 已闭环。
- 需要注意：computer use 截图频率高、体积大，必须复用"重复字节不新增文件"
  的去重策略；视口缩放到 XGA 级别（参考 Anthropic `scale_coordinates`）可显著
  降低 token 与存储成本，也是模型视觉理解的关键工程点。

### 4.4 权限、审批与审计边界

- 浏览器能力是**有副作用的执行能力**，与 Shell 同级对待：`BrowserAuthorizationFacts`
  应包含 action、URL（navigate/eval 目标）、坐标等类型化事实，进入 Authorizer 与
  Guardrail；高危动作（eval 任意 JS、导航到任意 URL、下载文件）建议默认 Ask。
- Chrome 进程生命周期与 Shell 类似：由 Adapter 管理，支持超时、取消（CDP 命令可
  在等待响应时被 `CancellationToken` 中断，但已注入页面的 JS 无法被常规取消，需
  `Runtime.terminateExecution` 或接受其自然结束）、输出上限与进程树清理。
- 审计：navigate URL、eval 表达式、click 坐标、注入文本进入审计；截图正文不进
  Conversation/Journal（v0.17.0 已有先例）。
- 明确不承诺强沙盒：Chrome 以当前用户权限运行，`Runtime.evaluate` 可执行任意 JS，
  Chrome 漏洞或用户 profile 中的凭据都可能被触及；文档与 UI 不得把浏览器能力表述为
  隔离环境。可与 `--user-data-dir` 临时 profile 结合降低对用户真实 Chrome 数据的
  影响。

### 4.5 进程拓扑与生命周期（遵循 AGENTS.md 硬约束）

- 浏览器能力**不改变产品进程拓扑**：不引入"浏览器作为 Session 子进程"的绑定；
  Chrome 实例由 Runtime 侧工具 Adapter 按需管理（启动、复用、回收），同一时刻
  可多个 Session 共享一个浏览器进程（通过 `Target.createBrowserContext` 隔离
  incognito 上下文），或每个 Session 独立 profile —— 这是装配策略，不是架构变化。
- 桌面客户端与 Host 双进程模型保持不变；浏览器能力完全属于 Runtime 侧。

## 五、开放问题与后续建议

1. **范围**：首版建议限定"本地 Chrome/Chromium 浏览器内"（可含 headless 与 headed
   两种模式，headed 便于用户肉眼观察）；整个桌面的 computer use（任意应用）依赖
   macOS Accessibility，是另一条技术路线，建议单独立项评估。
2. **浏览器来源**：使用系统安装的 Chrome（需发现路径，macOS 为
   `/Applications/Google Chrome.app/...`）还是 chromiumoxide fetcher 自动下载
   Chromium？后者可复现性强、不依赖用户机器，但体积大。
3. **连接目标**：launch 新实例 vs 连接用户已开的调试端口（`--remote-debugging-port`）。
   首版建议 launch 独立实例 + 临时 user-data-dir，避免与用户日常浏览器纠缠。
4. **模型兼容**：动作枚举与坐标语义是否严格对齐 Anthropic computer use（利于模型
   复用），还是采用 Playwright MCP 风格的工具名（利于既有浏览器工具训练数据）。
5. **AX 树与 DOM 的选择**：模型"理解页面"用 `Accessibility.getFullAXTree`（语义化、
   省 token）还是 `Runtime.evaluate` 提取精简 DOM / 自定义可访问性视图；可参考
   Playwright MCP 的 `browser_accessibility_tree` 与各开源项目的自定义 AXT 方案。
6. **与 MCP / Skills 的关系**：浏览器能力先作为内置一等工具，还是等 MCP 机制
   落地后作为可插拔能力；本仓库 MCP 调研尚未定稿，建议浏览器工具不阻塞、不等待。
7. **何时立项**：`v0.18.0` 进行中（M6 待确认、M7 未开始），浏览器能力建议排在
   `v0.18.0` 验收归档之后，作为独立版本或并入"Skills/MCP/插件"路线后的能力扩展。

## 六、调研依据

- [Chrome DevTools Protocol 官方文档](https://chromedevtools.github.io/devtools-protocol/)
- [CDP protocol JSON（browser_protocol + js_protocol）](https://github.com/ChromeDevTools/devtools-protocol)
- [Anthropic computer-use-demo（computer.py 源码）](https://github.com/anthropics/anthropic-quickstarts/tree/main/computer-use-demo)
- [OpenAI Computer use 指南](https://developers.openai.com/api/docs/guides/tools-computer-use)
- [Playwright MCP](https://github.com/microsoft/playwright-mcp)
- [chromiumoxide（Rust CDP 库）](https://github.com/mattsse/chromiumoxide)
- 本仓库 `v0.17.0` 工具图片回传设计（`docs/versions/v0.17.0/功能设计.md`）
- 本仓库 MCP 机制讨论（`docs/inputs/260821-MCP机制讨论与Agent实现调研.md`）
