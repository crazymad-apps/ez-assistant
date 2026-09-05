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
- `SessionToolImage` 预览在重验 Session 边界、普通文件及 MIME/内容身份后直接返回已落盘原图；
  完整图片解码只发生在首次写入，不在预览请求中重新解码或实时生成缩略图。普通附件列表仍可使用
  独立的持久化缩略图路由。
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
- Workspace 缺省文件能力由 Runtime 根据 Session 冻结的主目录和附加目录派生；Host 不因 Workspace
  注册、恢复或目录编辑创建、合并或改写普通 `permissions.json`。该文件只保存用户显式规则；缺失文件
  是合法空作用域。兼容恢复只在完整七条 `default-workspace-*` 规则逐字段匹配历史系统模板时移除它们，
  任一规则被修改、文档无效或文件安全诊断异常都原样保留。

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
- Host 的 ModelServiceFactory 根据 Runtime 已编译的 provider/protocol/capabilities、endpoint、credential 和
  transport 构造具体 OpenAI-compatible 服务，不自行保存第二份模型配置或选择状态。
- Chat Completions 的 `provider = "vllm"` 显式装配 vLLM 方言；`provider = "local"` 仍使用
  普通 OpenAI-compatible，Host 不根据 loopback endpoint 猜测具体服务实现。只有模型能力声明
  reasoning 时才启用该方言的 reasoning 解码和 effort wire 映射。
- 私有 Ratatui Demo 可以查询/reload 配置、选择 model key、创建 Session 和显式触发连接验证；
  v0.10.0 又增加全部 Session 列表、持久化 Run 查询、应用选中模型和归档/恢复入口。连接验证
  必须先提示真实请求与可能费用。该状态只服务交互，不成为 Runtime 业务事实源。
- 进程关闭由 Runtime 的有界受控关闭保证最终返回；Host 同时观察信号任务与连接子任务，
  区分预期取消和异常退出。

## v0.10.0 本地存储装配边界

- Host 私有 `storage` 模块实现 SQLite 与每 Session Conversation JSONL 的物理格式、一致提交和
  启动恢复；这些路径、表、offset、generation 和 worker 命令不进入应用协议。
- Conversation JSONL offset 索引同时维护物理消息位置与产品展示位置：隐藏 Runtime User Message
  保留物理 offset，但不生成 display offset；分页总数、Around 的 display ordinal 与 Recall FTS
  均只使用产品可见消息。SQLite `sessions.message_count` 继续表示物理 JSONL 消息数。
- 单一命名阻塞线程从打开到关闭独占 `rusqlite::Connection` 和文件 I/O；Tokio 侧通过有界命令
  队列等待结果，不能直接执行阻塞数据库操作。
- `storage/worker` 按适配职责拆分：`command` 定义类型化队列消息，`client` 实现异步
  `RuntimeStore` 代理与 worker 生命周期，`thread` 只负责在阻塞线程上把命令分发给
  `StorageEngine`。拆分不能改变命令顺序、背压、连接所有权或关闭语义。
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
- Attachment Blob 文件名使用 `<内容身份摘要>.<原始安全扩展名>`；摘要身份仍由原始文件名与文件
  字节共同决定。Session 可读路径可以继续使用软链接，但其真实目标必须保留扩展名，以便 macOS
  LaunchServices 等系统类型识别机制正确打开文件。Host 启动时原地迁移旧的无扩展名 Blob，并只
  重建指向该已知旧目标的稳定视图；不得为系统打开另建需要生命周期管理的展示副本。
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
- Workspace 的缺省信任由 Runtime 从 Session 冻结目录派生，Workspace `permissions.json` 只承载
  显式规则。Session 私有目录与附件目录的默认信任仍由 Host 生成普通 Session 权限文档：Session
  创建、Fork 和启动恢复只在文件缺失时生成，Plan/Build 默认读取当前 Session 私有目录及附件目录，
  Build 默认变更 Session 私有目录；不得覆盖或合并既有文件，不得授权 Session 父目录或其他 Session。
  Session 权限文件位于可写私有目录中，因此 Host infrastructure policy 必须拒绝结构化
  Write/Edit/Delete 直接修改该控制文件；Shell 边界仍按当前用户权限如实说明。
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

## v0.14.1 正式产品投影与私有 Web Demo 退役

- 本节是当前构建约束，并覆盖上文 v0.11.0 与 v0.13.0 中仅用于当时验收的私有 Web Demo 描述；
  历史段落保留用于解释版本演进，不代表当前 Host 仍提供这些入口。
- 主 Conversation 与 child Conversation 均通过 `assistant-protocol` 的有界产品投影读取；child
  使用 `GetChildTaskView`、统一 Conversation 游标分页和 Tool Detail，不提供整份 Conversation
  Host-private 命令，也不暴露 JSONL 路径。
- `web-demo` Cargo feature、`--web-demo` CLI、`/demo` 路由、静态资源、Host 状态字段、capability
  和专用验收测试已从当前产品删除。默认构建和 `--all-features` 构建都不得重新带入该能力；
  `--web-demo` 必须被参数解析明确拒绝。
- Host 只薄适配正式 child view/list/cancel、Conversation 分页、Tool Detail 和 SSE 事件；子审批
  继续归入父 Session 的唯一 Runtime 审批队列，不建立 Host 或 Desktop 私有审批状态。
- child Usage 只投影到对应任务和子视图；Host 不计算父子聚合值，右侧当前上下文继续以主 Session
  为 owner。

## v0.16.0 随包模型目录边界

- 只读目录位于 `apps/runtime-host/resources/model-catalog.json`，通过 `include_str!` 随 Host
  构建；运行期不读取用户副本、不写回、不远程更新。
- Host 在打开 Store 和对外发布 endpoint 前严格解析目录，并把已校验目录注入 Runtime；目录错误
  直接阻止启动，不能回退为猜测能力。
- Host 模型工厂只消费 Runtime 已编译的 provider、protocol、capabilities 和 Route 连接信息，
  不持有第二份目录或模型选择状态。

## v0.17.0 会话 Tool Image 存储基础

- Host 从 Session ID 唯一构造 `data/sessions/<session-id>/tool-images/`，并在创建、Fork 和
  启动恢复时与 `attachments/`、`private/` 一并准备；不为该可推导路径新增 SQLite 列。
- Host 图片模块保存通过签名嗅探、实际 MIME、静态性、完整解码、像素、边长、宽高比和
  64 MiB 字节上限校验的原图字节。稳定文件名为 `<sha256>.<规范扩展名>`，同目录随机
  `.part` 经刷新后以不覆盖提交发布，目标已存在时必须重验普通文件、哈希、MIME 和解码限制。
- 相同 Session 的相同字节串行或并发物化只保留一个稳定文件；稳定文件设为只读只是防误操作，
  不是 OS 沙箱。结构化文件工具对任意 Session 的 `tool-images/` 执行 Write/Edit/Delete 必须
  fail-closed，Shell 仍保持当前用户权限边界。
- 启动恢复在 staged append 与 pending exchange 修复后扫描图片：删除普通 `.part`；从当前主、
  child Conversation generation 标记引用；仅当全部 Conversation 可读时删除合法未引用稳定文件。
  Conversation 不可读时保守保留稳定文件；非法名称、非普通文件、损坏文件或缺失引用将 Session
  标记为资源异常，不自动覆盖或猜测修复。
- Fork 在目标 Session 内复制并重新校验字节，不创建跨 Session hard link、symlink、全局缓存或
  引用计数；复制失败不得提交半完成的新 Session。Session 删除自然回收本 Session 全部图片。
- M2 的 `SessionImageMaterializer` 组合 `agent-tools-local` 有界二进制读取与 Host 图片校验/存储，
  每个 Run 只绑定当前 Session 的 `tool-images/` 根。`read_image` 是否注册完全服从 Runtime 已编译
  的精确路由能力；Host 不按 Provider/model 名称再次猜测。
- Tool Image 模型预处理每次重新验证 Session 根形状、单层引用、普通文件、哈希、实际 MIME 和
  解码限制，再生成请求期有界 JPEG；请求期转换不写入 `tool-images/`，也不制造第二份稳定图片。
- `inspect_images` 的本地路径在 Runtime 完成统一文件 Read 授权后进入 Host 公共图片预处理；
  Host 支持相对路径解析后的绝对普通文件，不再要求它属于附件 Registry。结果只进入当次辅助
  `ModelCallContext`，不得复制到 `attachments/` 或 `tool-images/`，也不得产生产品资源投影。
- 随包模型目录 schema v2 为 DashScope `qwen3.8-max`、Moonshot `kimi-k3`/`k3` 和 DeepSeek
  `deepseek-v4-flash-vision-exp` 的 Chat 路由启用 `AggregatedUserInput`。DeepSeek 的非视觉
  Flash/Pro、GLM 和未真实回证的 OpenAI Chat 路由保持 `Unsupported`，即使其附件图片输入能力为真
  也不注册 `read_image`。
- Host 模型工厂对 `openai_chat_completions` 与 `openai_responses` 做穷尽式协议分派，分别构造
  `OpenAiChatCompletionsService` 和 `OpenAiResponsesService`；不能先请求一个协议再失败回退另一个。
  Responses 使用同一条已校验 Route 的 `/responses`，credential、Transport、超时和 wire observer
  继续复用共享基础设施。
- 随包目录对 Responses 只启用已验证的精确路由：DeepSeek Flash/Pro（reasoning、无图片）、
  DeepSeek `deepseek-v4-flash-vision-exp`（原生 content-parts function output 图片）、DashScope
  Qwen `qwen3.8-max`（聚合工具图片）和 Moonshot Kimi `k3`（原生 function output 图片）。GLM
  保持未启用，OpenAI 官方方言只供 fixture 和用户显式配置，未进入默认目录。
- Host 只按精确 provider/protocol/model 构造具名 Responses Adapter；route fingerprint 在已校验
  endpoint 与 model 上确定性计算。切换 endpoint、provider、protocol 或 model 不得把旧 opaque
  payload 发往新路由，也不得根据 Provider 一次成功/失败修改目录能力。
- Tool Image 预览路由同时编码 Session owner 和可选 child task ID，再由 Runtime 回查可靠引用；
  WebView 提交的 message/resource ID 不能直接转换为任意路径。
- 每次预览重新验证单层文件名、普通文件、内容哈希、实际 MIME 与公共解码限制，并只在内存生成
  最长边 320 的 JPEG。不得在 `tool-images/` 写 `.thumbnail.jpg` 或其他派生文件。
- Session Tool Image 不提供 native-path 路由能力；系统打开和目录揭示继续只适用于普通工具文件。
- Runtime Home 整体移动后，Host 启动恢复只重定位可严格验证为旧 Session/Workspace 固定布局的
  Runtime 私有绝对目录，并同步迁移有效 Session 权限文件中位于旧 `private/`、`attachments/`
  根下的 File matcher；用户 Workspace 目录不变，非法或无法归属的路径不得猜测修复。

## v0.18.0 M1 WorkPlan 存储边界

- 正式 SQLite 使用 `session_work_plans` 保存每 Session 当前唯一 WorkPlan；item 作为单列 JSON 整体
  读写，不拆成可单项查询表。旧数据库只增量创建空表，不扫描、推断或回填既有 Session。
- Host Store 在单一 Immediate transaction 内实现 revision CAS 与 `last_operation_id` 幂等；重复
  operation 返回首次结果，旧 revision 不得覆盖新计划。JSON 和不透明 ID 在恢复边界重新校验。
- Fork 在创建目标 Session 的同一 SQLite transaction 内复制当前 WorkPlan 为 revision 1；源和目标
  后续独立更新。Session 删除依赖已启用的外键级联清理，归档不删除或改写计划。
- WorkPlan load/mutate/clear 继续经单一有界阻塞 Store worker 执行；Host 不复制 Session 内存状态，
  不实现 `update_plan` 工具、Agent 决策或 Goal/自动续跑状态机。
- 迁移、CAS、重启、Fork、clear 与删除测试只能使用 `TempDir` 隔离 Runtime Home，不读写用户实际库。

## v0.18.0 M2 Goal 恢复存储边界

- 正式 SQLite 增量创建 `session_goals`，每 Session 最多一行，保存 objective message ID、有序载荷 JSON、
  版本化 hash、三态、暂停原因、generation/turn、冻结预算、累计使用和时间事实；Session 删除通过外键级联。
- Host 启动在加载 Runtime 投影前，以单一 Immediate transaction 将所有 Running Goal 持久切为
  Paused(RecoveryRequired) 并将 generation 递增一次；已经暂停或完成的 Goal 重复启动不得再次递增。
- Host 只解析 SQLite 形状和稳定 ID/数值；objective hash、预算常量、状态机和 latch 由 Runtime 领域层
  校验。非法 JSON 或存储范围立即 fail-closed，不把损坏行降级为空 Goal。
- 易失 Store 与 SQLite Store 保持相同的恢复暂停语义。M2 不提供独立 Goal insert/update 通用端口；
  Goal 的首次持久创建必须等 M3 与 Goal Input/Run 在一个高层原子操作中完成，不能先写控制器再入队。
- 所有 schema 与恢复验证仅使用 `TempDir`/内存 fixture，不打开、迁移或写入用户实际 Runtime Home。

## v0.18.0 M3 Goal 首次接受存储边界

- `inputs` 增量增加 origin、GoalId、generation 与 turn 列，并以约束和唯一索引保证 Runtime
  continuation binding 的基本形状；旧行安全缺省为 User/no binding。迁移只增加列和索引，
  不扫描 Conversation 猜测 Goal 归属。
- Host 在单一 SQLite Immediate transaction 内插入 `session_goals`、Goal-bound Input、首次 Run
  并更新 Session；任一步失败整体回滚，幂等 key 命中优先返回首次事实。Host 只校验跨表身份与
  存储形状，不复制 Goal 状态机或注入内容决策。
- 加载时重新校验 Input origin、binding 与 queued UserMessage 的来源/可见性组合；Goal 孤儿、
  generation 超前、同 Session 多条待领取 Goal Input 或非法消息组合均 fail-closed。
- 迁移、原子回滚、幂等与损坏行测试继续只使用隔离 `TempDir` SQLite，不访问用户 Runtime Home。

## v0.18.0 M4 Goal 事务与恢复存储边界

- `settle_run` 的 staged append 同时携带可选 Goal 终态 effect；M9 新增的活动 Run 内提交复用同一
  staged append/Immediate transaction 预检，在 Run 保持 running 时原子追加候选消息、隐藏推进消息和
  可选 Goal progress。两条操作都必须先验证跨表身份、CAS 与 generation，再发布 JSONL。
- 自动 Goal progress 不再插入后继 Input/Run。Volatile Store 与 SQLite Store 使用相同活动 Run、消息
  顺序和世代门禁，Store 结果是 Runtime 内存投影的权威输入。
- Stop 在单一 transaction 中暂停 Goal、递增 generation、删除该 Goal 旧 queued Runtime Input/Run，
  并记录活动 Run cancel intent；Clear 只删除控制器行。`inputs.goal_id` 是历史绑定而非控制器所有权
  外键，因此 Clear 后历史 Input 继续可读；Session 外键仍负责永久删除时的整体级联。
- Resume 新消息、无消息 continuation 和 held Input 复用分别有显式 Store 操作；held 路径只更新已有
  Input binding/queued message injection 和 Run，不产生重复队列项。历史重入的 Conversation rewrite
  与 Goal RecoveryRequired/generation 转换同事务提交。
- 启动恢复暂停 Running Goal；自动推进不再产生 queued Runtime continuation。Fork 在目标
  Session/WorkPlan 创建事务中按 Runtime 已验证的前缀条件插入新 GoalId 的 Forked 快照，Host 不自行
  推断 objective 是否属于前缀。
- 所有恢复、回滚、Fork、Stop/Resume/Clear 与 schema 用例只使用隔离 `TempDir`/内存 Store，不打开或
  迁移用户实际 Runtime Home。

## v0.18.0 M5 Goal/WorkPlan 正式 Host 边界

- `/commands` 薄分发 ClearWorkPlan、StopGoal、ResumeGoal 与 ClearGoal，业务准入和状态转换仍由
  `assistant-runtime` 持有；Host 只做 DTO 路由与稳定 HTTP status 映射，不复制 Goal 状态机。
- WorkPlan/Goal changed 复用正式 SSE RuntimeEvent 通道作为 invalidation。断流、未知事件或 sequence
  gap 继续要求客户端重新读取 SessionView，不能用事件负载拼装权威快照。
- 正式离线 Host E2E 使用 Fake Provider 和隔离 `TempDir` Runtime Home，覆盖 WorkPlan 工具更新、
  三次 continuation、blocked/resume、Stop 取消以及进程杀死/重启后的 RecoveryRequired/显式恢复；
  测试不访问用户 Runtime Home，也不向真实 Provider 发请求。

## 验证

```bash
cargo tree -p assistant-runtime-host --depth 3
cargo check -p assistant-runtime-host -p assistant-runtime -p assistant-protocol
cargo run -p assistant-runtime-host -- --help
cargo clippy -p assistant-runtime-host --all-targets --all-features -- -D warnings
```

## v0.21.0 M0 Host 运行框架

- `HostSupervisor` 是 `runtime-host` 内唯一的进程级长期子系统 owner，持有根关闭令牌、每个子系统的
  child token、受跟踪 task group 和 Host 子系统关闭 deadline；它不保存 Session、Run、Device、
  托管或 Store 的第二份业务状态。
- Desktop HTTP 是关键子系统：未进入关闭流程却返回、报错或 panic 时触发整体 Host 受控关闭。
  Device Gateway 与 Speech 等后续可选子系统使用降级策略，其失败不得停止 Desktop/Runtime 主链；
  M0 不实现自动重启，后续如需要必须基于真实故障和明确退避策略另行设计。
- connection reader/writer 等短任务不直接登记到进程 Supervisor，必须由所属 transport/Gateway 的
  有界 task group 拥有、取消和等待，避免单连接故障被误判为进程级故障。
- 关闭顺序为：根令牌停止 Host 新入口并限时回收 Host 子系统，再显式调用 Runtime shutdown 结算
  Run、flush/join Store；即使 Host 子系统异常或超时也不得跳过 Runtime shutdown。超时后的 task abort
  是 Host 资源回收兜底，不替代 Runtime 自己的可靠恢复语义。
- Ctrl-C 不再由临时 detach/abort 的信号任务管理；HostSupervisor 直接观察信号。HTTP ShutdownRuntime
  Command 继续通过根令牌结束 Host，并复用 Runtime shutdown 的幂等语义。

## v0.21.0 M2 Device Gateway 运行边界

- `DeviceGatewayService` 是 HostSupervisor 管理的可降级长期子系统；它持有安装 TLS 身份、
  mDNS、WSS listener、配对窗口、pending 候选、在线连接、心跳和当前输出偏好，但不保存第二份
  Device/Session/Conversation 权威状态。
- 安装身份只在首次启用设备接入时写入 Runtime Home `device/`；私钥和元数据使用私有权限与
  同目录发布，损坏、类型异常或权限过宽时 listener fail-closed，Desktop HTTP/Runtime 仍可用。
- WSS 仅提供 `/device`；未配对连接只能走 5 分钟窗口内的 SPAKE2 流程。设备在双向 PAKE
  确认后才生成 Ed25519 长期密钥，Host 只在 bind commit 通过后调用 Runtime 登记公钥。
- mDNS 只发布定位、版本、证书指纹和配对可用性线索，不构成认证。配对窗口开关或自然过期
  都要刷新发现投影。同一 Device ID 新连接替换旧连接；撤销后立即通知在线连接，后续挑战认证
  统一 fail-closed。
- Device Gateway Command 是 Desktop 到 Host 的交互意图，不是 Runtime Agent 指令。Host 组合 Runtime
  的稳定设备记录和 Gateway 在线/pending 状态返回单一快照；关闭 Gateway 不改写已配对设备或
  PC 输出托管事实。
- M2 只协商当前真实可用的文字/显示能力，屏蔽语音输入、PCM 输出和播放取消；设备业务
  文字输入与回复派发属于 M3，不得在 M2 未认证连接上提前开放。

## v0.21.0 M3 Device 文字交互边界

- 已认证 WSS 的 `text_input` 只携带稳定 `client_input_id`、正文和本轮输出偏好；Device ID 来自
  当前认证连接，目标 Session 由 Host Device Channel Router 从 Runtime 权威 Session 投影选择，设备不能提交任意
  Session、reply target、附件、Goal 或 Skill 意图。
- Host 把设备输入转换为 `SubmitSessionInputRequest`，并与 Desktop Command 一样调用统一
  `submit_session_input`；可靠接受后才返回 `input_accepted`；
  相同 Device/client identity 重试由 Runtime 返回首次 Input/Run，不在 Gateway 建幂等缓存。
- `DeviceChannelOutputDispatcher` 是 Runtime 传输中立输出端口的 Host Adapter。它只弱绑定 Gateway
  在线资源，避免形成 Runtime/Gateway 所有权环，也不保存正文、delivery、connection ID 或离线队列。
- 每个 Device delivery 在发送时独立核对当前认证连接、协商能力和偏好来源；来源轮次使用冻结偏好，
  PC 托管使用目标连接当前偏好。离线、能力不足或有界连接队列满只产生脱敏诊断，不修改已结算 Run、
  规范 Conversation 或其他 delivery。
- Device 只接收 `input_accepted`、有限状态和允许的 `text_output`；完整规范输入/输出继续由 Desktop
  HTTP/SSE 与 Conversation 投影保证。M3 不开放 PCM、ASR、TTS、播放或 Desktop 设备管理 UI。

## v0.21.0 M4 Desktop Gateway 观察边界

- Host 的 Gateway Command 继续组合 Runtime 稳定设备/托管事实与 Gateway 易失状态，并将结果作为
  Desktop 唯一设备快照；重命名、撤销和托管先由 Runtime 完成权威变更，再刷新组合投影。
- Gateway 内部 broadcast 只发布 `Changed` 失效信号；HTTP SSE 将它与 Runtime 事件并行投影为
  `device_gateway_event`。任一广播 lag 都沿用 `stream_gap` 后断开，要求 Desktop 重连并读取快照。
- 接入启停、配对窗口、候选变化、认证连接、在线偏好和稳定设备变更都会触发失效通知。Host 不将
  这些易失事实写入 Runtime Store，也不要求 Desktop 按心跳或本地时钟猜测状态。
- M4 的 ASR/TTS 投影固定为 unavailable；实际 SpeechService 生命周期、配置解析、降级原因和 PCM
  处理仍属于 M5/M6，不能因 UI 已能展示状态而提前声称语音能力可用。

## v0.21.0 M5 PCM 与 Host ASR 边界

- `SpeechService` 是 HostSupervisor 管理的可降级平行子系统，与 DeviceGateway 共享同一个安全
  `RuntimeConfigSource`，但独立持有 Provider、请求任务、取消和 readiness；它不进入 Runtime Core，
  也不从 Gateway 连接生命周期派生服务生命周期。
- Host 私有 `[speech.asr]` 配置编译 DashScope Adapter；缺失、非法或不可构造时只把 ASR 投影为
  unavailable，Desktop、Runtime 和设备文字能力继续可用。M5 只校验 TTS 配置形状并保留平行 SPI，
  在 M6 Adapter 落地前不得投影为 ready。
- 已认证连接每次只允许一个 `ReceivingPcm/Recognizing` 上行周期。16-byte 网络序 header 与固定
  PCM16 LE、16 kHz、mono、20 ms payload 必须逐帧校验；stream、sequence、格式、尾序号或 60 秒
  字节上限不一致时 fail-closed，不把媒体帧包装成 Runtime Event。
- `listen_stop` 封口后才调用整句 ASR；成功 transcript 经非空与长度校验后，以原
  `client_input_id` 调用统一 `submit_session_input`。取消、空结果和识别失败不创建 Input；
  同连接及重连重试最终都由 Runtime 的既有幂等事实返回首次 Input/Run。
- 开发构建只有在配置显式绝对 `speech.debug_audio_directory` 时才尽力写入上行 PCM；Release 强制
  不保存。写入失败不影响 ASR 或 Runtime 结果，不增加数据库、TTL、容量管理或 UI。

## v0.21.0 M6 TTS 与设备播放边界

- `[speech.tts]` 与 ASR 通过平行 SPI 编译 DashScope Adapter；配置缺失或非法只投影 TTS unavailable，
  不影响 Runtime、Desktop 或设备文字能力。Provider 返回的音频 URL 必须是受信 DashScope OSS HTTPS
  地址（测试只额外允许 loopback），PCM 下载受超时、取消和 60 秒字节上限约束。
- Gateway 为每次 `speak` 先向目标认证连接预留队列位，再合成该片段；同一连接使用最多 20 个
  位置的有界 FIFO
  严格保持 Tool Call 顺序，前一段播放结束后才开始下一段。队列满、目标离线或能力不足时该次
  Tool 明确失败，不建立无界缓冲或离线补播。
- 下行使用与上行共享的 16-byte 网络序 header，direction kind 为 `2`，PCM16 LE/16 kHz/mono 按
  20 ms、640-byte 固定帧发送；尾帧补零，`playback_start.sample_count` 给出真实样本数。慢连接通过
  有界命令队列和单帧发送超时终止本地播放，不建立无界媒体队列。
- `playback_cancel`、新设备输入、连接替换/断开和 Host 关闭只撤销该连接当前及待播队列，
  不调用 Runtime Run cancel。`playback_start/frame/end`、PCM 与当前播放状态都是 Host 易失传输状态，
  不写入 Runtime Store 或公共 SSE。
- H5 播放端把 PCM 连续重采样后送入单一长期 AudioWorklet 流，由渲染线程执行 120 ms 首缓冲和
  80 ms 下溢恢复，并在相邻 `speak` 片段之间保留 160 ms 气口；本地 marker 排空后才显示完成。
  浏览器本地容量或处理器故障不得冒充用户取消并清空 Host 后续播放队列。
- 输出周期结束仍无成功 `speak` 时明确返回 `no_speak_text`，不得退回朗读完整 Assistant 正文。开发构建只在
  显式绝对 debug 目录下尽力保存 `*-tts.pcm`，Release 默认不保存。

## SpeechService 健康、预算与恢复边界

- ASR/TTS 配置表分别反序列化和编译，单侧字段缺失、类型错误或 Provider 构造失败只令该能力
  unavailable；整个 TOML 或共享 speech 配置无效时整体 fail-closed。
- ready 表示 Adapter 已可用或最近调用成功，不代表后台探测已验证远端。auth、timeout、请求失败
  和 panic 只使所属能力 degraded；空 transcript 不代表服务故障，取消与本地满载不改变健康。
- degraded 仍允许下一次显式请求；成功恢复 ready。没有后台收费探测、失败请求重放或自动重试循环，
  因此不引入隐式退避等待。配置不可用须显式 reload；actor 退出后 unavailable，进程监督仍不自动重启。
- 入口以独立 Semaphore 限制 ASR 4 个、TTS 2 个已接纳请求，命令排队、执行和完成回收共用许可；
  满载返回 Host 内部 Busy，并映射现有设备错误，不新增 wire code。ASR 单句不超过 1,920,000 byte，
  TTS 文本不超过 120 字；现有 Adapter 仍限制 JSON 1 MiB、TTS 输出 PCM 1,920,000 byte。
- 上述上限约束 SpeechService 持有的请求载荷和任务，不是整个 Host RSS 上限。ASR WAV/Base64/JSON、
  HTTP 缓冲和已交付 Gateway 播放队列另有内存成本；Gateway 队列不能因 Provider 许可释放而取消上限。
- 默认 30 秒、最大 120 秒的配置超时包围完整请求（包含 TTS 合成和下载），取消包围 debug 音频 I/O。
  调用方取消或丢弃结果时停止等待并回收 Provider future；不得自动重放可能已发送的请求。
- 配置重载保留在途请求的原快照；完成时用 Provider Arc 身份核对是否仍为当前实例，旧结果只回复
  原调用方，不覆盖新配置的健康。不新增 generation、持久化状态或服务端会话。
- actor 拥有两组有界 JoinSet；先停止入口、置 unavailable、取消队列及在途请求，再等待回收。
  强制 abort/panic 也使最终快照不可用；Gateway/Runtime/终端不复制这份权威状态。

## Device Gateway 播放接纳与媒体诊断

S3.2 联调的内部播放接纳与诊断约束：

- `PreparedPlayback` 仅拥有尚未交付的连接预留；合成失败、PTT、连接替换、确认超时或调用方
  Future 丢弃均取消原槽位。内部 `StartPlayback` oneshot 必须在 connection owner 实际附加 PCM
  后才成功；仅进入 command channel 不算接纳，重复/缺失/已取消槽位不得覆盖现有输出。
- 每连接 FIFO 保持最多 20 段，确认等待最多 1 秒。PCM 接纳后由连接 owner 管理取消和发送；
  已形成成功回执优先于短音频完成时取消的令牌。没有 detached 取消 watcher、自动重放或 wire ACK。
- `speak` 成功只表示 Host 队列接纳，不表示 WSS 全部发送、终端消费或扬声器物理完成。
- `media_diagnostics` 对 request/run/input/device ID 做固定长度 SHA-256 摘要关联；普通诊断仅有
  时间、静态原因、深度及字节计数，无 PCM、正文、密钥或完整标识。Provider 记录整次请求耗时，
  Gateway 记录下行首帧和发送完成；不得据此声称具备 Provider 首包或声学完成指标。
- 部署沿用用户明确选择的 Runtime Home；联调自动化使用独立临时 Home 和本地测试 Provider，
  不借验收重启覆盖正在运行的用户配置或数据库。

## v0.21.0 M1 设备与 Channel 存储边界

- SQLite `devices` 只保存 Device ID、当前展示名、Ed25519 原始公钥、paired/revoked 生命周期及配对、
  更新、撤销时间；不保存配对码、连接 Token、设备私钥、在线状态、能力、当前偏好或媒体数据。
- `sessions.pc_output_device_id` 只保存 Controller 到设备的可选稳定关系；恢复时通过 Device Registry
  解析当前名称。设置托管在单一 Immediate transaction 内重新核对 Controller 与 paired 设备；撤销
  设备在同一事务清除全部托管引用，历史 Input 不随撤销删除或改写。
- `inputs.channel_source_json` 随既有 Input/首次 Run 接受事务保存通用来源；旧 User 行缺失时按 Desktop
  Text/Text 读取，旧 Runtime 行保持无外部来源。Device User Input 在同一接受事务内再次核对目标
  Session active 且设备仍 paired，避免认证后到落账前的撤销竞态；Controller 选择只属于 Host Router。
- Goal continuation 所需的报告返回依据只随既有 Goal/Input binding 保存；旧行缺失按
  `SessionDefault` 解释。Host Store 不建立 Channel、Reply 或 Delivery 表，也不保存 OutputCycle；
  投递失败不会触发 SQLite Conversation 或 Run 回滚。
- M1 当时尚未接入的正式设备连接、能力协商和在线投影已由 M2 Device Gateway 接入
  HostSupervisor；业务文字输入和回复 dispatcher 仍属于 M3。M1/M2 的 schema、重启、撤销和历史
  来源测试只使用隔离 `TempDir`。

## v0.21.0 M9 活动 Run 续跑存储边界

- Host Store 通过单个 `commit_run_continuation` 业务操作，在既有 JSONL staged append 与 SQLite
  transaction 中提交活动 Run 的消息增量及可选 Goal effect；Run、Input 和 Queue 生命周期保持不变。
- 该操作要求 Run 仍为 running、Session/Run 所有权一致且无 pending Tool Exchange；消息进入
  `run_message_refs` 并绑定当前 AgentExecution 的最终 step，不增加 Loop ID、revision 或新表。
- 进程在 Loop 间中断时只从已提交 Conversation/Goal 事实恢复，并按既有规则终结非终态 Run；Host
  不持久 continuation 决定，也不为 Goal 或 Speech 建立恢复队列。
- Context replacement 写入的新 JSONL generation 必须包含完整产品 Conversation：保留全部旧轮次，
  在原样 recent tail 之前插入新的 ContextSummary。SQLite `message_count`、产品 offset/Recall 索引和
  分页均基于这份完整正文；旧 generation 只能在新正文与 SQLite 切换成功后 best-effort 清理。
- replacement 的 System prefix 与 recent tail 必须和当前权威正文严格一致，Store 不接受模糊匹配或
  客户端补偿拼接。Runtime 重启后从最后一个 ContextSummary 派生 Agent 上下文，因此不增加第二份
  Conversation、边界表、revision 或同版本兼容字段。

## v0.19.0 内部上下文兼容存储

- Host Store 把 `InternalContext` 与所在规范 UserMessage 一起写入既有 Conversation JSONL 和
  queued message JSON；不增加 Provider request-only 信封表或第二份消息正文。
- 恢复校验同时接受旧 `Injected` 与新 `InternalContext`；新结构的 boundary、kind、retention key
  在 `agent-types` 反序列化边界校验，非法正文令恢复 fail-closed。
- Recall/FTS、产品分页、导出和展示继续排除两类内部 Part，不能把结构化内部正文索引成用户内容。

## v0.19.0 本地 Skill 扫描与名称状态存储

- Host 只扫描 Session 冻结的有序 Workspace 根目录和用户 Home 各自的
  `.ez-assistant/skills`、`.agents/skills`；每个 Root 只认直接子目录，不沿工作区祖先或其他客户端
  目录扩展。
- `serde-saphyr` 仅在 Host 反序列化 frontmatter。YAML、必填字段或边界不可确定时跳过候选；
  可选字段无法采用时使用缺省并生成诊断，不修改源文件，也不执行包内脚本。
- 发现扫描只读取 `SKILL.md`，对候选数、定义/frontmatter 大小和候选特殊类型设固定上限；
  候选项只保留源目录，不枚举或读取普通资源，Root 不可完整遍历令整次投影不可用。
- SQLite `skill_name_states` 只保存通过校验的名称、布尔开关和更新时间；单行 upsert 使用
  Immediate transaction。测试只打开 `TempDir` 或内存数据库，不访问用户实际 Runtime Home。
- 新 Session 的 `skill_catalog_json` 保存 Runtime 编译出的精确 `SKILL.md` 正文、definition digest 和
  共享源目录；普通资源树不枚举、不复制、不建立索引，Runtime Home 与 Session 目录均不创建
  Skill 暂存或私有副本。
- 普通资源按 Skill 指令通过既有文件/Shell 工具读取共享 Root 中的当前文件，并继续服从具体
  工具的路径与权限规则；Host 不增加 Skill 级权限门禁。
- SQLite 迁移给旧 Session 写入 `legacy_unavailable` 缺省；恢复只校验 Catalog 结构与 revision，
  不因共享资源变化拒绝恢复。Fork 只复制 SQLite 中的 Catalog 事实，Session 删除不触碰共享源文件。

## v0.19.0 用户 Skill Activation 持久化

- `inputs.skill_activation_json` 保存 Input 自身的冻结 Activation 关联；`skill_activations` 保存可按
  Session/Conversation 顺序恢复的 ledger。用户 Input 接受事务依次写入 Input、首次 Run 和 ledger，
  任一步失败都不得留下部分事实。
- Host 校验 user Activation 的 Session/Input/Run/Message、owner、trigger 和 `skill:<name>` retention
  关系，但不执行 Skill 级权限判断；真实工具调用继续进入既有 Authorizer 与审批链。
- 启动恢复同时读取 Input 关联与 ledger；Runtime 还会按 activation id 核对两份结构完全一致，并验证
  Catalog revision、名称和 definition digest。旧 Input 缘由缺少字段时按无 Activation 读取。
- Queue 取消、Goal 清理、历史尾段替换和 Session 删除同步删除不再属于规范 Conversation 的 ledger；
  Fork 事务只插入前缀内已改绑目标 Session 的 Activation，不建立 Skill 目录或复制共享文件。
- `list_skills`/`set_skill_enabled` 通过正式 HTTP Command 调用 Runtime；SSE 只发布
  `skill_settings_changed` 失效提示，不成为开关或 Catalog 的第二份权威状态。

## v0.19.0 模型 Skill Activation 原子恢复

- parent/child pending exchange 的 ready 载荷同时冻结完整 Tool Results、可选隐藏 Activation Message
  和 Activation ledger；旧版本只含 Tool Result 数组的载荷继续兼容读取。
- staged append 把 Tool Call/Result、隐藏 Runtime UserMessage、相同 Run 全局 step 引用和 ledger 写入
  同一次 SQLite transaction；只有 JSONL 已发布且 SQLite finalize 成功后才清理 pending。
- 启动恢复按同一 ready 载荷重放全部事实，不能只恢复成功 Tool Result 而遗漏其 Skill Activation；
  owner、Session、parent Run、message id、Model trigger 与 `skill:<name>` retention 关系均需交叉校验。
- parent 与 child 复用同一原子提交协议，但分别写入各自 Conversation owner；共享 Skill 包仍留在
  扫描 Root，不因激活复制到 Runtime Home 或 Session 目录。

## v0.22.0 M0 兼容迁移基线

- SQLite 对 Workspace label/附加目录、Session 冻结附加目录、可空唯一 materialization key、自动
  标题 pending 和旁路 usage 聚合采用 additive migration；重复初始化必须幂等。
- 旧 Workspace label 由主目录 basename 回填，Workspace 与 Session 附加目录默认为空，旧 Session
  不补自动标题。目录 JSON 在 Store 边界按绝对路径、顺序、去重和数量上限 fail-closed。
- 迁移与损坏数据测试只使用内存 SQLite 或隔离 `TempDir`，不得以版本开发为由打开或修改用户实际
  Runtime Home。

## v0.22.0 M1 Workspace 多目录装配边界

- Host 在 Store 边界对完整 Workspace 表单做 canonicalize，并验证目录绝对、存在、可读、UTF-8、
  数量上限与无 canonical 重复；活动 Workspace 主目录全局唯一，附加目录允许跨 Workspace 共享。
- 新 Session 的系统上下文明确写入 label、主目录语义、有序附加目录和“工作区不是强沙盒”；根
  `AGENTS.md` 按主目录到附加目录顺序读取并标注来源根。Skill 扫描使用相同根顺序，根内仍保持
  `.ez-assistant` 优先于 `.agents`，用户来源顺序不变。
- SQLite 保存 Workspace 当前目录与 Session 冻结目录两类事实。恢复 Session 只要求关联 Workspace
  仍存在，不用当前 Workspace 目录覆盖或否定历史冻结环境；相关迁移与恢复测试只使用隔离目录。


## v0.24.0 Session 文件资源与模型交付约定

- Session 资源根的可用身份由 Runtime 提供；Host 按 locator 校验主目录、附加目录序号和精确 Session 私有根，负责有界列举、媒体识别、预览及本地操作前解析。每次读取重新验证 canonical 路径、符号链接和根归属，不把展示路径或缓存当作授权。
- 未知扩展名经过内容检查可作为普通文本预览；二进制不得因改后缀被当作文本。图片与 PDF 必须通过类型校验和字节上限，不返回任意本地路径给网页。
- 新建/清空 Session 冻结一次 file URI 交付约定；继续使用原 System Prompt，Fork 复用冻结指令并重建目标 Session 目录部分。模型请求捕获测试覆盖这些边界，不以静态 Prompt 断言替代真实 Provider 文件交付验收。
- 浏览器、用户终端、页面 LRU 和右栏快照属于 Desktop，不进入 Host 的 Session/Run/Conversation 或 Agent 工具定义。
