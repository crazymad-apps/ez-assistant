# desktop 模块约束

## 模块定位

`apps/desktop` 是 Tauri 2 桌面应用，负责 WebView UI 与 macOS/桌面系统能力，并通过
`assistant-protocol` 和 Runtime Client 把 UI 意图发送给独立 Runtime Host。

修改本模块前必须同时阅读：

- 前端：[`docs/specs/前端编程规范.md`](../specs/前端编程规范.md)
- Rust：[`docs/specs/Rust编程规范.md`](../specs/Rust编程规范.md)

## 技术栈

- WebView：Vanilla TypeScript、Vite、`@tauri-apps/api`。
- 原生层：Tauri 2、Rust。
- macOS 渲染：系统 `WKWebView`。
- 当前不引入前端框架和全局状态库；需要引入时先记录架构决策。

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
- 关闭主窗口或退出桌面客户端不得默认终止 Runtime；只有明确的“退出 Assistant”或 Runtime
  管理动作才触发受控关闭流程。
- 桌面专属数据（窗口大小、悬浮球位置、快捷键）不进入 Agent 会话模型。
- Runtime 配置（模型、工具、调度）不应散落到前端 localStorage。
- capability 按实际需要最小开放；修改窗口标签时同步检查 capability 绑定。
- 自定义 command 需要明确请求/响应类型、错误映射和权限语义。

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
