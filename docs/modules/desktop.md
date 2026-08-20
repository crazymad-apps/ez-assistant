# desktop 模块约束

## 模块定位

`apps/desktop` 是 Tauri 2 桌面应用，负责 WebView UI 与 macOS/桌面系统能力，并通过
`assistant-protocol` 和 Runtime Client 把 UI 意图发送给独立 Runtime Host。

修改本模块前必须同时阅读：

- 前端：[`docs/specs/前端编程规范.md`](../specs/前端编程规范.md)
- Rust：[`docs/specs/Rust编程规范.md`](../specs/Rust编程规范.md)

## 技术栈

- WebView：React、MobX、Sass、TypeScript、Vite、`@tauri-apps/api`。
- 原生层：Tauri 2、Rust。
- macOS 渲染：系统 `WKWebView`。
- 当前前端架构决策见 [`v0.14.0 技术方案`](../versions/v0.14.0/技术方案.md)；更换视图框架、
  状态方案或样式体系时必须先更新技术决策。

## 职责

- 主窗口、未来的悬浮球、托盘、快捷键和窗口生命周期。
- 文件/目录选择、通知、屏幕捕获、TTS 和系统权限申请。
- Tauri command 参数适配、Runtime Client 调用、Runtime 事件转发和错误展示转换。
- 应用启动、隐藏、退出以及 Runtime 的连接、启动请求和受控停止入口。

## 核心约束

- `src-tauri` 依赖 `assistant-protocol` 和 Runtime Client，不直接依赖或装配
  `assistant-runtime`；Tauri command 必须薄，不实现会话或 Agent Loop。
- Tauri Rust 启动层从 Runtime Home 私有发现文件取得 loopback 地址和进程级
  Token，将它们作为启动配置注入受信任 WebView。WebView 内的 HTTP Runtime Client 可
  直接调用 Runtime Command、Upload 和事件流，不要求每个请求都经 Tauri command 代理。
- 远程连接是默认关闭的可选能力；后续如显式启用，同一 WebView Runtime Client 改用
  HTTPS 地址和远程身份，不另造命令、事件或上传客户端。
- WebView 中的 Runtime Token 只存内存，不进入 URL、`localStorage`、日志或普通前端事件；
  配套 CSP、受控导航和精确 WebView Origin 白名单。
- 事件订阅使用能携带 Authorization header 的 `fetch` streaming SSE，不在 URL 中传 Token。
- 纯浏览器直连本地 Runtime 保留为未来显式开放的可选模式，默认关闭；它可通过
  独立的 Origin 白名单和授权边界直连，不强制引入 companion bridge。
- WebView 不直接访问文件系统、模型 API、数据库或 Shell；必须通过受控 Tauri command。
- 关闭主窗口或退出桌面客户端不得默认终止 Runtime；只有明确执行“停止 Runtime”，或在当次退出
  确认中选择“同时停止 Runtime”，才触发受控关闭流程。“退出 Assistant”不作为独立动作。
- 桌面专属数据（窗口大小、悬浮球位置、快捷键）不进入 Agent 会话模型。
- Runtime 配置（模型、工具、调度）不应散落到前端 localStorage。
- capability 按实际需要最小开放；修改窗口标签时同步检查 capability 绑定。
- 自定义 command 需要明确请求/响应类型、错误映射和权限语义。
- 附件或工具文件不支持应用内预览时按正常能力回退展示友好说明，不直接暴露 Runtime 原始错误；
  “使用系统应用打开”和“在目录中打开”都只向窄 Tauri command 提交稳定资源身份，由原生层
  回查 Runtime 后执行，WebView 不得提交任意本地路径。
- 子任务树和子 Agent 消息复用主 Conversation 的 Message、Reasoning、Tool Detail 与分页组件；
  进入子任务只替换中栏消息 owner，不创建子 Session、子 Composer 或第二个右侧上下文 owner。
- 子 Agent 审批继续消费父 Session 的唯一 pending approval 投影，并使用同一底部审批工作区；
  主视图和子视图不得同时复制一份阻塞列表。子任务自身 Usage 放在任务树和子视图，UI 不聚合父子
  Usage。
- 正式 Desktop 不依赖已退役的私有 Web Demo、整份 Conversation 命令或 Demo capability。
- WebView 生产源码与测试代码物理分离：`src/` 不放测试文件；单元/组件测试、测试支持代码和
  Playwright 场景分别集中在 `tests/unit/`、`tests/support/` 和 `tests/e2e/`。

## v0.17.0 Tool Image 详情边界

- 工具详情预览请求必须携带完整 `ConversationOwner`；child task ID 进入独立 URL 命名空间，不能
  只用 Session/message ID 猜测归属。
- `SessionToolImage` 只在对应工具详情中显示有界图片预览。它不进入附件 Store、Context Panel、
  通用产物列表或导出，也不展示系统打开、目录揭示和复制物理路径操作。
- Host 返回缺失或损坏错误后，当前详情把对应项标记为不可用；关闭或切换详情时清除此 UI 派生态，
  不修改可靠 Conversation。
- 多路径文件审批必须逐条展示 `ToolApprovalSubject::Files.paths`，不能只显示工具名或第一条路径；
  Runtime 提供的 Session/Workspace 持久批准会精确保存每条路径，Desktop 不自行折叠授权范围。

## 不应放在本模块的内容

- Agent Loop、模型 Provider 和工具选择策略。
- 多会话调度、Run 状态机、定时任务与持久化业务。
- 为 UI 方便而复制一套协议类型并产生不同语义。
- 通过前端直接执行 Shell 或读取任意本地文件。

## Harness 验证

```bash
cd apps/desktop
npm run build
npm run tauri -- build --no-bundle
```

涉及窗口、托盘、快捷键、屏幕捕获、TTS 或系统权限时，必须补充 macOS 实机验证并说明权限状态。
