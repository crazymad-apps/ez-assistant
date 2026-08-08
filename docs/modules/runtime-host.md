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
- 每条连接拥有的 reader/writer/event 子任务必须被显式观察；正常取消是连接清理，意外
  `JoinError`/panic 必须上报为 Host 连接错误，不能用忽略返回值的方式静默吞掉。
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
- Runtime 业务数据库实体、跨层暴露的持久化记录类型或只为 Demo 服务的公共 Client crate。Host
  可以私有实现 RuntimeStore 的 SQLite/文件 Adapter，但不得因此持有第二份 Session/Run 状态机。
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

## v0.9.0 配置装配边界

- 默认 Runtime Home 是当前用户主目录下的 `~/.ez-assistant`；Host 在 bootstrap 时将其解析为
  绝对路径。`--runtime-home` 仍只接受绝对路径，不建立旧目录回退或并行配置源。
- Host 从 Runtime Home 安全读取唯一 `config.toml`，将解析结果交给 Runtime；不再从环境变量、
  `.env` 或模型 CLI 参数读取 endpoint、model 和 credential。
- Host 的 ModelServiceFactory 根据 Runtime 已编译的 provider/profile、endpoint、credential 和
  transport 构造具体 OpenAI-compatible 服务，不自行保存第二份模型配置或选择状态。
- 私有 Ratatui Demo 可以查询/reload 配置、选择 model key、创建 Session 和显式触发连接验证；
  v0.10.0 又增加全部 Session 列表、持久化 Run 查询、应用选中模型和归档/恢复入口。连接验证
  必须先提示真实请求与可能费用。该状态只服务交互，不成为 Runtime 业务事实源。
- 进程关闭由 Runtime 的有界受控关闭保证最终返回；Host 同时观察信号任务与连接子任务，
  区分预期取消和异常退出。

## v0.10.0 本地存储装配边界

- Host 私有 `storage` 模块实现 SQLite 与每 Session Conversation JSONL 的物理格式、一致提交和
  启动恢复；这些路径、表、offset、generation 和 worker 命令不进入应用协议。
- 单一命名阻塞线程从打开到关闭独占 `rusqlite::Connection` 和文件 I/O；Tokio 侧通过有界命令
  队列等待结果，不能直接执行阻塞数据库操作。
- `rusqlite` 只属于 Host，并使用关闭默认 feature 的 `bundled` SQLite；`assistant-runtime` 只依赖
  RuntimeStore 端口。
- Serve 启动先取得 endpoint 单实例所有权，再打开 Store 并等待 Runtime 完成结构化恢复；只有
  `AssistantRuntime::open` 和配置加载成功后才进入 accept loop，因此客户端不会观察半恢复状态。
- Runtime Host 把同一个 `LocalRuntimeStore` 注入 Runtime。受控关闭先等待 Run supervisor 结算，
  再由 Runtime flush 并 join 存储 worker；bootstrap 失败也显式关闭已经启动的 worker。
- M2 已由私有 `input_state` 与 `run_state` 模块接管 Input 准入、Session 内幂等 key、持久 queue
  order、Run attempt 和重启投影；Host 仍只实现 RuntimeStore，不持有第二份队列执行状态。
- M3 的私有 `tool_exchange` 模块实现 begun/ready、整批 staged append 和启动修复；begun 恢复生成
  outcome unknown 错误结果，ready 恢复保留已记录结果，完成正文提交后立即删除 pending，且从不
  自动重新执行工具。存储 worker panic、reply channel 关闭、join 失败与 Runtime 侧关闭超时统一
  表现为可观察错误；只有 Shutdown 命令进入 worker 队列后才关闭后续 Store 准入。
- M4 的私有 `session_management` 模块实现归档/恢复、模型 key 更新和历史重新输入：归档及模型切换
  在 SQLite 写入时再次复核 lifecycle、queued Input、非终结 Run 和 pending exchange；历史重新
  输入先写完整新 generation，再在单一 SQLite 事务中 CAS 切换正文、级联删除尾段 Input/Run/引用
  并创建新的 committed Input/Accepted Run。事务提交后旧 generation 只做 best-effort 清理，清理
  失败不能把已经成功的权威切换回报成业务失败。
- v0.10.0 人工验收继续复用 Host 现有私有 Ratatui Demo；自动化验收客户端只驱动正式 Host 与临时
  loopback fake Provider。不得为验收另建第二套 Runtime、Session/Run 状态机或产品 fake 模式。
- 离线端到端回归使用 `tempfile` Runtime Home、临时 loopback Provider 和两次真实 Host
  进程；客户端只使用现有私有 frame 协议。验收结束后以只读方式核对 SQLite 与权威 JSONL，
  不读写 `~/.ez-assistant`，也不连接真实 Provider。

## 验证

```bash
cargo tree -p assistant-runtime-host --depth 3
cargo check -p assistant-runtime-host -p assistant-runtime -p assistant-protocol
cargo run -p assistant-runtime-host -- --help
cargo clippy -p assistant-runtime-host --all-targets --all-features -- -D warnings
```
