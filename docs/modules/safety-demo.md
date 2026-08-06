# safety-demo 模块约束

## 模块定位

`tools/safety-demo` 是开发者显式启动的安全策略与真实工具执行验证宿主。它装配真实或
scripted Provider、Agent Core、`agent-tools-local`、内存 Session/Run/Journal、模式
策略、一次性审批、审计和静态 Web 页面。

它不属于桌面产品进程、正式 `assistant-runtime`、sidecar、daemon 或长期本地服务。
产品 crate 不得依赖本工具，其私有类型不得成为产品兼容承诺。

修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`前端编程规范`](../specs/前端编程规范.md)（修改 `public/**` 时）
- [`Agent 系统技术架构`](agent-system.md)
- [`agent-core 模块约束`](agent-core.md)
- [`agent-tools 模块约束`](agent-tools.md)
- [`agent-tools-local 模块约束`](agent-tools-local.md)

## 依赖与所有权

- 本工具是顶层开发目标，可向下依赖正式 Agent crate、Provider Adapter、
  `agent-tools-local` 和测试所需的 `agent-testkit` dev-dependency。
- `assistant-runtime`、`assistant-protocol`、desktop 和 `runtime-harness` 不依赖本
  工具，本工具也不复用它们的私有 Session、Run、Journal 或 Context 编排。
- DemoSession、DemoRun、approval ID、audit、HTTP DTO 和页面投影全部留在 binary
  私有模块。
- 一个进程只持有一个 Session 和一个活动 Run；页面不是权威状态源。
- 上层状态锁只保护短临界区，任何模型、审批等待、文件、Shell 或网络 future 都不得
  持锁 `.await`。

## 安全边界

- 只允许绑定 `127.0.0.1`，端口默认由 OS 分配。
- 本 Demo 不实现访问令牌鉴权；启动 URL 可直接打开，API 也不要求 Bearer。
- Host、Origin 和 POST 来源严格校验，不开放通用 CORS；这些限制只降低浏览器跨站误触
  风险，不构成对本机其他进程的身份认证。
- 静态页和 API 统一 `Cache-Control: no-store`，并设置限制性 CSP。
- credential、完整文件内容和完整 Shell 输出不得进入日志或审计。
- Shell 以当前用户权限运行，可能绕过文件名单；工作目录和逻辑路径不得描述为系统
  沙盒。
- Shell 审批、活动和结果必须展示 `managed` / `detached`；`detached` 成功仅表示启动
  命令完成交接，不表示服务健康。Run 取消、Session reset 或 Demo 退出不会停止已经
  交接的进程，页面必须固定提示该风险。

## Run、模式与审批

- Plan/Build 与 Ask/Auto 是两个独立维度；每个 Run 在启动时冻结二者。
- Plan Capability Policy 必须先给每个有效调用明确 Allow/Deny；Auto fallback 不得
  覆盖 Plan Deny。
- Build 使用 typed trusted rules；同一规则集 Allow 优先 Deny，未命中才进入 Ask
  审批或 Auto AllowAll。
- Ask 只提供 Allow once/Deny；不持久化规则、不批量审批、不设置审批超时。
- 取消必须与审批等待竞争并移除 pending approval，不留下仍可操作卡片。
- Demo 默认 Guardrail 必须显式注入 ExecutionSpec，不得变成 Core 隐藏默认。
- 内存审计记录 resolved 展示事实、策略、审批和执行状态，不进入 AgentEvent、
  Conversation Journal 或正式 Runtime。

## HTTP 与页面

- snapshot 是页面恢复的权威状态；SSE 只提供带递增 seq 的增量通知。
- `RunProgress` 只可投影页面实时展示所需的脱敏字段：模型正文/reasoning 增量、
  Tool Call 标识、Shell 输出通道和 Guardrail 事实；不得透传 ToolProposed 原始
  arguments，避免把 write/edit 文件内容复制到活动日志。
- seq gap、断线或重连时禁用写动作并重新获取 snapshot，前端不推测缺失终态。
- 页面建立 SSE 后必须再次校准 snapshot，封闭首次 snapshot 与订阅建立之间的事件
  窗口；旧 snapshot 不得覆盖已经应用的更高 seq。旧端口连续恢复失败后进入稳定断开
  状态，不无限自动重连。
- 正文、reasoning 和 Shell 输出等高频分片必须在页面内合并并局部更新，不能逐分片
  重建完整页面或请求 snapshot；结构性事件和终态仍以 snapshot 校准。
- 桌面双栏视口固定在页面可视高度内，对话列表与检查器分别滚动；只有用户原本位于
  对话底部时，新增流式内容才自动跟随。窄屏允许退化为整体页面滚动以保证内容可达。
- reset 只在无活动 Run 和无 pending approval 时执行，并显式清理旧临时工作区。
- 静态页面使用原生 HTML/CSS/JavaScript，不新增 Node/Vite 工程。
- 模型文本、路径、command、stdout/stderr、错误和原始 JSON 必须通过
  `textContent` 渲染，不能使用 `innerHTML` 注入不可信数据。
- 页面可以投影运行、审批和审计状态，但不得复制 Core Policy 或 Run 权威状态机。
- Build + Auto 的高风险确认只存在页面内存；刷新后必须重新确认。审批只提供
  Allow once/Deny，不在 DOM 中保存另一份截断 command。

## 配置与凭据

- `--workdir` 必传且启动时冻结；Web API 不允许修改。
- `--port` 缺省 `0`；非 loopback 地址不能由公开构造或 CLI 表达。
- 只有真实对话入口加载 `.env` 和 Provider credential；测试与静态验证保持离线。
- 真实 DeepSeek 服务由 Demo 装配层构造显式 `ModelRequestConfig`，冻结 reasoning 和
  `thinking: { type: enabled }`；Core 只逐 Step 机械投影配置，不依赖 Provider 默认模式或
  DeepSeek 名称分支。
- 配置错误尽早失败且脱敏，不能打印 API Key 或带 credential 的环境摘要。
- Session 临时工作区在 reset/shutdown 时显式 close；异常 drop 只承诺尽力清理。

## 不应放在本模块的内容

- 正式 Runtime Approval Service/SPI、审批持久化或产品工作模式契约。
- `assistant-protocol` DTO、Tauri command、桌面会话或设置 UI。
- 数据库、日志导出、多 Session 调度，或由 Demo/Session 持有并管理的后台服务。
- AgentExecution 总超时、新增资源预算、No Progress Guardrail。
- Shell AST、命令改写、OS 沙盒、TTY、stdin、后台进程句柄或服务健康管理。

## 验证

M0 骨架阶段：

```bash
cargo metadata --format-version 1
cargo tree -p safety-demo --depth 2
cargo check -p safety-demo
cargo fmt --all --check
```

功能落地后：

```bash
cargo test -p safety-demo
cargo clippy -p safety-demo --all-targets --all-features -- -D warnings
```

- Router、Runtime、审批和模式自动测试必须完全离线。
- 文件测试使用临时目录，Shell 使用无破坏受控命令。
- 真实 Provider 与完整人工闭环需显式启动，不进入默认测试。
- 页面必须核对 loopback 入口、四种模式、审批、取消、断线恢复和可访问性。
