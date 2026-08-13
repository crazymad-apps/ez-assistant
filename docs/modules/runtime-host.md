# runtime-host 模块约束

## 模块定位

`apps/runtime-host` 是 EZ Assistant 正式产品的 Runtime Host 进程入口。它装配
`assistant-runtime`、具体 Agent/Provider/Tool 能力和 HTTP 应用协议，但不持有 Session、Run 或
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
- 承载 HTTP 路由、连接生命周期、流式上传、SSE 事件投影和安全边界。
- 把跨进程 Command 转交 Runtime，把 Runtime 快照、响应和事件投影到连接。
- 处理进程信号、停止接收新连接、调用 Runtime 受控关闭并清理自己拥有的 endpoint。
- 输出脱敏的启动、关联 ID、状态、耗时和错误信息。

## 数据与状态边界

- Session、Conversation、Run、Journal、取消和调度状态只存在于 `assistant-runtime`。
- Host 可以持有连接、传输缓冲、进程配置和具体资源句柄，但不能复制 Runtime 业务状态机。
- 跨层稳定语义使用 `assistant-protocol`；HTTP 路由、认证、请求体限制和 SSE 编码
  属于 Host 适配，不把 Web framework 类型或连接对象提升为公共 Protocol。
- 客户端连接断开只结束连接相关任务，不默认取消 Runtime Run 或关闭 Runtime。

## 传输与并发

- 当前产品通信统一为 HTTP：默认只启用本地 Host，并只绑定 IPv4 loopback 动态端口。
  后续可选远程 Host 默认关闭；如显式启用，必须使用 HTTPS 并复用同一组
  Command、SSE、Streaming Upload 路由与业务 DTO。
- 传输任务使用有界队列并明确背压；慢客户端不得反压 Provider、AgentExecution 或 Runtime
  supervisor。
- SSE 订阅者落后于有界 Runtime 广播时，Host 必须显式发送 `stream_gap` 控制事件并关闭
  当前流；客户端重新连接后从 Conversation、Run、Session 等权威快照恢复，不能在丢失事件后
  继续假装增量连续。该连接恢复不得取消业务 Run。
- 每条连接拥有的 reader/writer/event 子任务必须被显式观察；正常取消是连接清理，意外
  `JoinError`/panic 必须上报为 Host 连接错误，不能用忽略返回值的方式静默吞掉。
- Host 只负责连接并发，Session 内串行和跨 Session 并发由 Runtime 保证。
- 本地地址、访问 Token 和实例身份通过 Runtime Home 私有发现文件交付；其创建、
  权限、stale endpoint 和所有权判断以当前版本技术方案为准。

## 安全与日志

- credential 只用于构造具体 Provider，不进入应用协议、普通日志、Demo 输出或子进程环境。
- 本地 HTTP 请求必须通过每进程高强度 Bearer Token、Host/Origin 校验和精确
  CORS 白名单收窄边界。Tauri Rust 可将 Token 注入受信任 WebView，由 WebView Runtime Client
  直连 Host；Token 不进入 URL、持久前端存储、事件或日志。
- 普通日志不得记录完整 prompt、模型响应、文件内容、Shell 输出或工具参数。
- Host 通过 `RunToolFactory` 为每个 Run 同时冻结工具集和 Authorizer；未知或未匹配能力不得
  隐式放行，不能使用一份跨 Session 的可变 resolver 或全局可变权限状态。
- 默认 Host 注册正式文件与 Shell 工具，并统一交给 Runtime Authorizer 决策；不得因验证模式
  引入绕过权限规则或审批的 Allow All 产品路径。

## 不应放在本模块的内容

- Session/Run 的权威业务状态、Conversation Journal 或 Agent Loop。
- Tauri command、WebView 状态、窗口、托盘和桌面展示逻辑。
- Provider Codec、工具 Dispatcher、Context 算法或 Core Guardrail 实现。
- Runtime 业务数据库实体、跨层暴露的持久化记录类型或只为 Demo 服务的公共 Client crate。Host
  可以私有实现 RuntimeStore 的 SQLite/文件 Adapter，但不得因此持有第二份 Session/Run 状态机。
- 系统开机启动、崩溃拉起和 daemon 管理；这些能力需由后续平台版本明确设计。

## v0.8.0 实现边界

- 本节记录 v0.8.0 的历史实现事实；其 Unix Socket 传输已由 v0.11.0 统一 HTTP
  决策替代，不再是当前新增功能的约束。
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
- v0.10.0 自动化回归继续驱动正式 Host 与临时 loopback fake Provider；v0.11.0 已用默认关闭的
  私有 Web Demo 取代 Ratatui Demo。不得为验收另建第二套 Runtime、Session/Run 状态机或产品 fake 模式。
- 离线端到端回归使用 `tempfile` Runtime Home、临时 loopback Provider 和真实 Host
  进程；客户端只使用正式 HTTP Command/SSE/Streaming Upload 入口。验收结束后以只读方式核对 SQLite 与权威 JSONL，
  不读写 `~/.ez-assistant`，也不连接真实 Provider。

## v0.11.0 HTTP 与附件入口边界

- 删除 Unix Socket、自定义 length-prefixed frame 和握手双轨；项目尚未对外稳定发布，
  不为旧 wire 保留兼容 server/client。
- `POST /commands` 承载有界 JSON 命令，`GET /events` 使用 SSE，附件路由使用
  multipart 流式请求体；禁止把附件 Base64 或本地源路径塞进 Command。
- Host 在流式接收前调用 Runtime 准入，完成 staging 后再调用 Runtime 提交；Host 不持有
  Attachment 的第二份业务状态。
- 本地只绑定 `127.0.0.1:0`，所有路由都需进程级 Token。发现文件必须原子更新，
  退出时只清理仍属于本 `instance_id` 的文件。
- 默认只允许无 Origin 的原生客户端和精确登记的 Tauri WebView Origin，不允许通配符 CORS。
  纯浏览器本地直连是后续显式开启的可选能力，需独立授权和 Origin 配置。
- `serve` 删除 `--socket`，不提供任意 `--listen` 或公网绑定开关；本地客户端和验证客户端
  都从 Runtime Home `run/runtime.json` 发现地址与 Token。
- v0.11.0 用默认关闭的 feature-gated 私有 Web Demo 替代继续迁移 Ratatui Demo。Web Demo
  只在显式启动时由当前 loopback Host 提供简单静态页面，直接调用正式 HTTP API，
  不持有 Session/Run 第二份状态，不进入公共协议，也不引入前端框架或单独服务进程。
- v0.11.0 只实现本地 Host；远程能力默认关闭，其 TLS、用户认证、多租户和运维
  不在本版本范围。后续如实现可选远程 Runtime，不得另造一套业务 API。
- M1 的本地 Store 以用户目录 canonical path 作为 Workspace 身份，SQLite 保存 Workspace
  与 Session 绑定，Runtime Home 保存 Workspace 私有目录、Session 私有目录和 Session
  附件目录。Workspace 假删不得删除上述目录或用户目录；同一 canonical path 再次登记时
  恢复原记录。
- Host 的 `SessionEnvironmentFactory` 是稳定目录到冻结 `SystemPromptSnapshot` 的唯一
  装配点，只注入目录信息，不枚举 Workspace 文件。启动时在开放 HTTP 前为旧 Session 幂等
  补建 unbound 资源行和目录，不改写既有 Prompt、Conversation、model 或 lifecycle。
- M2 的存储 worker 串行提交 staging、内容 Blob、Session 稳定附件视图和 SQLite 元数据；
  Host 启动开放 HTTP 前修复缺失的已知视图，并把无法安全修复的附件标记为 `unavailable`。
- M3 不增加 Message/Part 表；File References 直接随完整 User Message 进入既有
  `queued_message_json` 和 Conversation JSONL，Host Store 只沿用现有原子提交与 generation 切换语义。
- v0.12.0 M3 由 Host 顶层固定装配 `agent-tools-local`；默认 `serve` 为每个 Run 注册
  标准文件工具和 Shell，再由 Runtime 唯一 Authorizer 完成 Plan 硬边界、
  分层规则和审批决策。Host 不保留“安全工具集/危险工具集”双轨，也不提供绕过审批的启动开关。
- v0.12.0 M4 在 `pending_tool_exchanges` 下增加级联的 `pending_tool_starts`，只保存
  receipt、call ID 和开始时间。正式 Tool Result 通过 staged append 提交并清理 pending 时，
  started 记录一并清理；不形成永久工具审计表。
- 启动恢复先完成 staged append，再修复 begun/ready pending：没有 started 的调用形成
  “重启前尚未执行”错误 Tool Result，已有 started 但没有可靠结果的调用形成
  `outcome_unknown`，ready 结果按实际成功或失败结算；恢复不重新执行工具。
- 相对路径固定解析到该 Session 冻结工作目录；绝对附件路径可供文件工具
  按需读取。结构化文件 Authorizer 对 Runtime Home 下任意 Session 的附件目录执行
  Write/Edit/Delete 均拒绝，但允许 Read/List/Search。该逻辑路径规则不能阻止 Shell、同一
  OS 用户或 symlink 目标绕过，因此不得描述为不可绕过的附件隔离。
- 新 Session 的基础 System Prompt 只要求按需使用本 Run 可用工具，不把某一种 Host 模式或
  工具名单冻结进正文；旧 Session 已持久化的 Prompt 不在恢复时改写。
- v0.11.0 M5 扩展私有 Web Demo，覆盖 Workspace 登记与选择、Session 创建、附件先上传后提交、
  历史 File References 可见投影、流式 Assistant 消息以及 Session 归档恢复。Demo 只读取
  正式 HTTP 快照和 SSE，不持有第二份业务状态。
- Web Demo 展示当前 Session 最近一次完整模型请求的 Input、Output、Provider Total 和
  Cached Input token：在线时读取 usage SSE，选择 Session、刷新或断线恢复时从权威
  Conversation 中最近的 Assistant usage 重建，不在页面自行估算或累计。
- Web Demo 按 `call_id` 实时投影 Tool proposed/running/output/completed 事件；每个模型 Step
  的临时 Assistant 输出在进入工具阶段后关闭，下一 Step 使用新的临时消息。Run 终结后仍以
  权威 Conversation 覆盖临时投影。Reasoning 使用独立可见区域消费实时 `reasoning_delta`，
  并从权威 Assistant Reasoning Part 恢复历史内容；模型 attempt/retry 状态和最终脱敏失败
  摘要同步展示。
- v0.12.0 M5 在同一私有 Web Demo 中增加 Plan/Build、Ask/Auto、权限显式重载和 pending
  approval 投影。页面只按 `available_decisions` 生成决策按钮；模式切换写入 Runtime，提交仍
  显式携带当次 variant，“继续调整计划”和“开始实施”只填充普通输入，不引入工作流协议。
- 页面刷新、SSE gap 或重连后，Demo 必须重新读取 Session、Run、Conversation、Attachment 和
  pending approval 权威快照；不得用页面内存恢复审批或推断当前变体。Permission reload 只展示
  Runtime 返回的文件状态和安全诊断，不在前端解析或合并权限规则。
- Web Demo 的高频正文、reasoning 和工具输出增量必须按浏览器帧合并，不能逐 chunk 重写累计
  全文或强制同步布局；历史快照应离屏构建并限制同时挂载的展示规模。展示裁剪只影响私有 Demo，
  不能修改 Runtime Conversation、持久化内容或公共协议。
- v0.11.0 正式端到端验收必须至少启动两次产品 Host，通过正式 HTTP 接口完成 Workspace、
  Attachment、File References、真实文件工具和归档恢复，并在关闭进程后只读核对 SQLite、
  Conversation JSONL、Blob 与稳定视图。不得用 Demo 私有状态替代该证据。
- 正式端到端验收必须在同一 Session 连续完成至少两次工具 Run，并核对 Conversation 中所有
  Message ID 唯一，防止只覆盖首个工具 Run 而遗漏跨 Run 的持久化冲突。
- 默认忽略的真实 LLM smoke 可以只读使用用户显式提供或 Runtime Home 中的模型配置，但所有
  Runtime 数据、Workspace 和测试文件必须位于临时目录；错误与 Host 输出不得包含配置源文本、
  API Key、访问 Token、测试文件内容、Prompt 或完整模型回复。

## v0.13.0 子任务存储基础

- SQLite 只保存 `child_tasks` 关系/状态以及 child 版 pending、started、staged append 临时事实；
  子 Conversation 继续使用规范 `ConversationMessage` JSONL，位于所属 Session 的
  `child-tasks/<child_task_id>/body-<generation>.jsonl`，正文不复制进 SQLite。
- 父 Run 与 child 共用同一 staged append 和 pending tool exchange 状态机。目标适配只选择表、
  所有权与正文路径；begin、started、ready、commit 和重启补偿算法不得复制成两套实现。
- `child_task_id` 全局唯一，一个父 Tool Call 只拥有一个 child；所有创建、读取、工具交换和结算
  都核对 Session 所有权。旧数据库通过兼容性新增表和投影校验升级，不改写旧 Session/Run 行。
- Host storage worker 继续独占 SQLite 与文件 I/O；M1 自动测试只使用隔离临时 Runtime Home，
  不启动真实子 Agent、不访问真实 Provider 或用户业务数据库。
- M2 由 Host 实现 `ChildTaskWorkspaceFactory`：在系统临时根创建每任务独占目录，Runtime 只获得
  lease 和 UTF-8 绝对路径；子终态可靠提交后 Drop lease 清理目录。该临时目录不进入 Runtime Home、
  SQLite 或公共协议，也不能替代 Workspace/Session 私有持久目录。
- M3 不增加第二套 Host 调度器：并发 permit、活动 child 与取消原因都由 Runtime 持有，Host 继续只
  装配临时目录和 Store Adapter。SQLite Run 错误码投影兼容 `timeout`/`cancelled`。
- M4 启动恢复严格先修 child staged append/内部 pending tool exchange，再中断非终态 child，最后
  根据 child 终态修复父 `delegate_task` 结果并中断父 Run；缺失关系不伪造 child，已完成 child
  从其最终规范 Assistant Message 重建父成功结果，任何步骤都不重放副作用。
- M4 的 `/commands`、`/events` 继续只是 Runtime 公共 child 查询/取消/事件的薄适配。完整 child
  Conversation 只通过 Host-private 查询服务 Web Demo 验收，不公开物理 JSONL 路径，也不提升为
  `assistant-protocol` 的产品 DTO。
- M5 的私有 Web Demo 使用公共 child list/cancel 与 SSE 事件，并仅用 Host-private child
  Conversation 查询重建展开内容；前端按父 Run 动态汇总父、子 usage，不向 Runtime 写入聚合字段。
- 父 Run 与 child 共用 Runtime 自动 Rolling Summary；Host Store 只提供 generation 原子切换和
  规范正文提交，不在存储层复制窗口判断、摘要策略或剩余预算计算。
- child 卡片折叠时不累计正文和工具输出 DOM；展开后从权威 Conversation 加载稳定内容，实时增量按
  animation frame 批量写入，工具输出仅保留有界展示尾段。页面刷新和 SSE gap 后均重新查询，不能
  根据本地事件推断终态。

## 验证

```bash
cargo tree -p assistant-runtime-host --depth 3
cargo check -p assistant-runtime-host -p assistant-runtime -p assistant-protocol
cargo run -p assistant-runtime-host -- --help
cargo clippy -p assistant-runtime-host --all-targets --all-features -- -D warnings
```
