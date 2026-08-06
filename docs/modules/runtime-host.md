# runtime-host 模块约束

## 模块定位

`apps/runtime-host` 是 EZ Assistant 正式产品的 Runtime Host 进程入口。它装配
`assistant-runtime`、具体 Agent/Provider/Tool 能力和本地进程通信，但不持有 Session、Run 或
Conversation 的第二份权威状态。

本模块不是 `tools/*` 验证宿主，也不因独立进程形态自动成为系统 daemon、LaunchAgent 或常驻
Worker 池。

修改前必须同时阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`assistant-runtime 模块约束`](assistant-runtime.md)。
- [`assistant-protocol 模块约束`](assistant-protocol.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- 解析并校验进程级启动配置，构造正式 `AssistantRuntime`。
- 装配具体 Provider、Agent Factory、工具 Adapter、Authorizer 和 Runtime 依赖。
- 承载平台相关的本地进程通信、连接生命周期、协议适配和安全边界。
- 把跨进程 Command 转交 Runtime，把 Runtime 快照、响应和事件投影到连接。
- 处理进程信号、停止接收新连接、调用 Runtime 受控关闭并清理自己拥有的 endpoint。
- 输出脱敏的启动、关联 ID、状态、耗时和错误信息。

## 数据与状态边界

- Session、Conversation、Run、Journal、取消和调度状态只存在于 `assistant-runtime`。
- Host 可以持有连接、传输缓冲、进程配置和具体资源句柄，但不能复制 Runtime 业务状态机。
- 跨层稳定语义使用 `assistant-protocol`；Unix Socket frame、握手 envelope 和 Demo 命令属于 Host
  私有 wire，不进入公共 Protocol。
- 客户端连接断开只结束连接相关任务，不默认取消 Runtime Run 或关闭 Runtime。

## 传输与并发

- `v0.8.0` 采用 macOS-first Unix Domain Socket，不监听本地 HTTP/TCP 端口。
- 传输任务使用有界队列并明确背压；慢客户端不得反压 Provider、AgentExecution 或 Runtime
  supervisor。
- Host 只负责连接并发，Session 内串行和跨 Session 并发由 Runtime 保证。
- socket 的创建、权限、所有权、stale endpoint 和删除规则以当前版本技术方案为准。

## 安全与日志

- credential 只用于构造具体 Provider，不进入应用协议、普通日志、Demo 输出或子进程环境。
- 普通日志不得记录完整 prompt、模型响应、文件内容、Shell 输出或工具参数。
- 工具默认权限由 Host 显式装配的 v0.8.0 `default_authorizer` 决定；未知或未匹配能力不得隐式
  放行。后续工作模式必须从单次 Run 的不可变模式快照同时生成模型提示与该 Run 的 Authorizer，
  不能把此默认值扩展成全局产品策略。
- `v0.8.0` 的私有 Demo 只允许低风险 `echo_text`，不得因此引入通用 Allow All 产品默认值。

## 不应放在本模块的内容

- Session/Run 的权威业务状态、Conversation Journal 或 Agent Loop。
- Tauri command、WebView 状态、窗口、托盘和桌面展示逻辑。
- Provider Codec、工具 Dispatcher、Context 算法或 Core Guardrail 实现。
- 数据库实体、正式持久化格式或只为 Demo 服务的公共 Client crate。
- 系统开机启动、崩溃拉起和 daemon 管理；这些能力需由后续平台版本明确设计。

## v0.8.0 实现边界

- M0 只建立 package、无副作用 `--help`、依赖方向和本文档。
- M5 实现 macOS-first Unix Domain Socket、私有 frame/握手、单活动客户端、命令/实时事件转发、
  owned endpoint、单实例识别、受控关闭和 feature-gated Ratatui Demo。TUI 只持有选择、输入、
  滚动和临时提示等展示状态，不能成为 Session、Conversation 或 Run 的第二事实源。
- endpoint 父目录必须为 `0700`，稳定 lock 文件与 socket 为 `0600`；Host 通过进程生命周期内
  持有的内核独占锁识别单实例。取得锁后可以替换同一私有目录中的 stale Unix Socket，但必须
  保留普通文件、目录、符号链接等未知类型；退出时只清理 inode 仍匹配本实例的 socket。
- M5 只装配 `echo_text`，其余已注册或未来工具默认拒绝；未引入文件、Shell、持久化、daemon、
  自动重连或事件回放。

## 验证

```bash
cargo tree -p assistant-runtime-host --depth 3
cargo check -p assistant-runtime-host -p assistant-runtime -p assistant-protocol
cargo run -p assistant-runtime-host -- --help
cargo clippy -p assistant-runtime-host --all-targets --all-features -- -D warnings
```
