# assistant-protocol 模块约束

## 模块定位

`crates/assistant-protocol` 定义 UI 与 Assistant Runtime 之间，以及确有跨层消费者的稳定应用契约。Agent Core 内部的模型事件、规范对话、执行状态和能力 trait 不因复用方便而进入本 crate。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 允许内容

- `SessionId`、`MessageId`、`RunId` 等应用级 newtype ID；`RunId` 由 Runtime 持有，不进入 Agent Core 执行输入。
- Command/request、事件、快照和分页查询 DTO。
- Runtime Host 就绪状态与 capabilities 投影；不得包含地址、Token 或进程私有句柄。
- 跨层共享的状态 enum、错误码和序列化约定。
- 文件与 Shell 工具的纯数据请求/响应类型。

## 禁止内容

- Tauri、Tokio task、数据库连接、Repository 实现。
- 模型 SDK、HTTP client、文件句柄、子进程句柄。
- UI 组件状态、窗口对象和展示文案。
- `Arc<Mutex<_>>`、trait object 或无法稳定序列化的运行时对象。
- 为单个模块内部实现服务的私有类型。
- Agent Core 的 `AgentExecution`、`ExecutionSpec`、`ModelEvent`、Provider 状态和 trait。

## 协议演进

- enum 序列化使用显式 tag，variant 名称视为兼容契约。
- 字段含义不能静默改变；需要改变时新增字段/variant并给出迁移路径。
- 新事件必须检查 Runtime 生产、Tauri 转发、前端消费、持久化和恢复。
- 可选字段必须区分“未知/未提供”和真实空值语义。
- 错误对外提供稳定代码和安全消息，内部错误链不直接序列化给 UI。

## v0.8.0 初始化契约

- 本节记录 v0.8.0 的历史契约；Unix Socket 握手和私有 frame 已在 v0.11.0 被统一
  HTTP 协议取代。
- `PROTOCOL_VERSION = 1` 只供 Runtime Host 私有握手比较应用契约版本，不表示 HTTP、WebSocket
  或其他传输协议。
- 公共 ID 包含 Session、Run、Message、Part 和 ToolCall；均为经过非空校验的透明字符串
  newtype，具体生成格式不构成契约。
- 公共命令只包含 create/list/get Session、start/get/cancel Run 和 shutdown；成功结果与事件使用
  显式 serde tag，失败使用稳定 `RuntimeErrorCode` 和脱敏消息。
- 公共快照只表达 Session 摘要以及 Run/Tool 的当前观察投影。完整规范 Conversation、pending
  exchange、Core/Provider 原始对象均不进入本 crate。
- request correlation ID、握手 envelope、Unix Socket、length-prefixed JSON frame 和 Demo 私有
  Conversation 查询属于 `apps/runtime-host`，不得因当前实现方便提升为公共 Protocol。
- 本版本不定义事件 sequence、replay cursor、持久化、Plan/Build、审批、Workspace、上传或记忆
  DTO。

## v0.9.0 配置与模型契约

- 项目仍处早期阶段，`PROTOCOL_VERSION` 保持 `1`，在当前 DTO 上直接演进；这里不存在所谓
  “V2 协议”或并行兼容代际。
- `ModelKey` 是用户配置 key，不是 Runtime 分配的 ID。创建 Session 可显式传入 key，未传时
  使用当前有效配置的默认 key；Session 摘要返回其冻结 key。
- 公共命令增加配置状态、模型列表/详情、显式 reload 和模型连接验证。所有配置投影均脱敏，
  credential 只能表示为 `api_key_configured`，不得进入响应、事件或错误。
- reload 的 Missing/Invalid/Degraded/Ready 是可诊断业务结果；连接验证返回稳定失败分类，
  原始 Provider body、认证 header 和内部错误链不进入协议。
- Runtime Home 文件路径可以作为本机诊断信息返回，但私有 wire、Demo 交互、模型协议 Adapter 和
  Runtime 内部配置对象仍不属于公共协议。

## v0.10.0 可靠输入契约

- 项目仍处早期阶段，直接以 `SubmitInput`、`CancelQueuedInput`、`ResumeSession` 和 `RetryRun`
  替换旧 `StartRun` 提交入口，不保留双轨协议；`PROTOCOL_VERSION` 仍为 `1`。
- `IdempotencyKey` 是客户端生成的非空有界 opaque key，只表达同一 Session 内提交意图幂等；协议
  不定义正文摘要、全局去重或模型 credential 语义。
- `RunSnapshot` 通过 `input_id` 和 `attempt` 表达同一输入的多次执行尝试；`SessionSummary` 只公开
  `queued_input_count` 与 `resume_required` 投影，不公开 queue order 或存储表结构。
- `Accepted` 只表示输入与首次 Run 已持久化，不表示 Run 已经活动；只有 `RunStarted` 才表示该 Run
  被 Session 执行器领取。重启后的队列必须经显式 `ResumeSession` 才整体继续。
- `SessionLifecycle` 区分 active 与 archived；`ListSessions` 缺省只返回 active，并可显式筛选
  archived/all。归档 Session 的详情、Conversation 和 Run 历史仍可查询。
- 公共命令增加 `ListRuns`、归档/恢复、空闲模型切换与从历史 User Message 重新输入；历史重新输入
  只接受应用层 MessageId、新正文和可选幂等 key，不暴露 generation、文件路径或被删除表行。
- `SessionArchived` 与 `SessionNotIdle` 是不同的稳定错误：前者表示只读生命周期，后者表示活动、
  排队或未结算事实阻止当前变更。

## v0.11.0 Workspace、附件与 HTTP 投影契约

- M1 新增 `WorkspaceId`、Workspace lifecycle/summary、登记/查询/活动列表/假删命令，
  `CreateSession` 和 `SessionSummary` 通过可选 `workspace_id` 表达创建时冻结绑定；协议不提供
  Session 换绑命令，也不暴露 `session_resources` 等物理存储结构。
- `RegisterWorkspaceResult.restored` 明确表达本次登记是否恢复已假删记录；旧结果缺字段时
  安全缺省为 `false`。Desktop 使用该事实避免恢复工作空间时额外创建空会话。
- `WorkspaceId` 和 `AttachmentId` 是应用层不透明 ID；附件投影只返回客户端需要的
  原始名称、稳定状态和可展示元数据，不暴露 Blob、staging、SQLite 或客户端源路径。
- Workspace 意图、Attachment 查询和 Session 绑定进入可序列化
  `RuntimeCommand`/`RuntimeCommandResult`；`SubmitInput` 和历史重新输入以缺省为空的有序
  `attachment_ids` 引用 Session Attachment。附件字节流不是命令 DTO，
  由 Host 的 HTTP 上传路由承载。
- 默认本地 Host 和后续可选远程 Host 共用 HTTP 路由及业务 DTO；远程能力本身
  默认关闭。`PROTOCOL_VERSION` 仍保持 `1`，由 `/capabilities` 返回；不保留 Unix Socket
  握手或并行 V2。
- `/health` 与 `/capabilities` 使用 `RuntimeHostHealth` 和 `RuntimeHostCapabilities`；
  后者只表达协议版本、资源上限和当前可用 transport 能力，不携带发现地址或访问 Token。
- HTTP method/path、Bearer 认证、Host/Origin 校验、multipart 解析和 SSE 连接属于
  Runtime Host 的 transport adapter。共享 crate 只提供其中稳定的 payload 和错误语义，
  不依赖 Web framework 或 HTTP client。
- File References 是持久化 User Message Part，保存 Agent 可见的原始名称和稳定可读
  路径；附件正文、Base64、自动解析结果和 Workspace 文件列表不进入 Part。
- `TokenUsageSnapshot` 只投影一次完整模型请求最终确认的 input、output、Provider total 和
  可选 cached input；`RuntimeEvent::UsageUpdated` 携带所属 Session、Run 和 step。事件允许
  丢失，客户端重建时从已持久化 Assistant Message usage 恢复最近一次模型请求用量。
- 模型调用观察新增 attempt 开始、建流前失败、重试等待和建流成功事件；失败只携带
  `ModelFailureKind`、attempt 和重试决定，不携带 Provider 原始消息。最终 Run 使用
  `ModelExecutionFailed` 稳定错误码和安全摘要，现有客户端仍可按普通 Run 失败处理。

## v0.13.0 子任务增量契约

- M1 先增加不透明 `ChildTaskId` 与独立生命周期状态，避免把 child 伪装成普通 Session 或 Run。
- M3 只为既有审批快照及 resolved/cancelled 事件增加可选 `child_task_id`；父 Run 自身审批省略该字段，
  保持旧客户端可忽略新增字段。`timeout` 与 `cancelled` 是可稳定持久化的 Runtime 错误分类。
- `delegate_task` 审批使用专门的 `Delegation` subject 展示受限 title/task 摘要，避免 UI 只能看到
  通用工具名；持久规则预览仍是只匹配工具名的 `General`，不把任务正文写入权限文件。
- M4 增加 child list/get/cancel、`ChildTaskSnapshot`、`ChildTaskNotFound` 和嵌套
  `RuntimeEvent::ChildTaskEvent`。嵌套 envelope 明确携带 Session、父 Run 和 child ID，payload
  覆盖生命周期、text/reasoning、usage 及工具活动；不得用父 Run 的 delta 冒充 child 事件。
- child 的完整 Conversation 仍不是公共协议 DTO。SSE 可以丢失，客户端必须用 list/get 和 Host
  私有 Conversation 查询重建；取消先形成持久请求事实，终态重复取消幂等返回当前快照。

## v0.16.0 图片、推理、队列与 usage 增量契约

- 附件投影增加实际 `media_type`，图片仍沿用统一 `AttachmentId` 和文件引用；图片字节、Base64、
  像素尺寸、缩略图路径和模型请求对象不进入公共协议。
- 模型与 Session 投影只公开图片输入、reasoning、可用 effort key/label、默认 effort 和辅助视觉
  模型引用等最小能力事实；Provider 的 wire value、thinking 私有字段和 credential 不进入协议。
- Session 保存 reasoning effort 意图，Run 快照保存启动时冻结值。模型切换与 effort 降级由 Runtime
  原子完成，Desktop 不根据 Provider 或模型名称自行映射。
- `resume_queued_input` 是队列恢复的统一命令，可整体恢复或指定一个排队输入优先执行；旧
  `resume_session` 仅保留一个版本的兼容转发并视为弃用，新 Desktop 与测试不得继续调用。
- `SessionUsageSnapshot` 同时投影最新一轮 input/output/cached input、会话累计 input/output/cached
  input，以及由这些权威计数计算的最新命中率和综合命中率。累计值在每次已结算模型请求时持久化，
  Desktop 不为重建会话统计反复扫描 Run 日志。
- 队列、usage、effort 和模型能力事件允许丢失；客户端重连后必须以 Session/Run 快照恢复，不把
  本地乐观状态当作权威事实。

## v0.17.0 Tool Image 产品投影

- `ToolFileResourceOrigin::SessionToolImage` 只标识工具详情中的会话私有图片来源；DTO 不携带
  Image Part、Session 根、外部源路径、Base64 或 ProviderState。
- `ToolFileResourceState::Available` 表示可靠 Conversation 中存在可解析引用。物理文件仍由 Host
  在预览时重新校验；失败后 Desktop 可在当前详情中展示 `Unavailable`，不能据此改写 Conversation。
- Tool Image 不进入 `ConversationFileReference`，因此不能被附件、Context Panel、通用产物或
  用户文件导出消费。
- `ToolApprovalSubject::Files` 只投影一次多路径文件调用已经解析的 operation 和有序绝对路径，
  供 Desktop 完整展示审批范围；它不携带文件正文。Session/Workspace 持久批准仍由 Runtime
  将每条路径保存为既有 exact File matcher，不新增权限文件 matcher 类型。

## v0.18.0 M1 WorkPlan ID 增量

- 新增不透明 `TodoItemId`，与其他应用 ID 一样校验非空、使用透明字符串 wire 形状，并进入统一
  TypeScript 导出。ID 不编码 Session、排序、状态或数据库主键策略。
- M1 只先稳定 item 身份；WorkPlan 快照、clear 命令、revision conflict 和事件在 v0.18.0 的应用协议
  闭环里程碑统一加入，不能在 Desktop 先建立非权威的并行 DTO。

## v0.18.0 M2 Goal ID 增量

- 新增不透明 `GoalId`，沿用透明字符串 wire 形状、非空校验和统一 TypeScript 导出；ID 不编码
  Session、generation、turn、状态或预算。
- M2 只稳定跨层身份。Goal 快照、预算投影、停止/恢复/清除命令和事件仍归 M5，Desktop 不得读取
  Runtime 私有 `StoredGoal` 或根据 `update_goal` Tool Result 推导产品状态。

## v0.18.0 M3 Goal 提交意图

- `SubmitInputRequest` 增加具有旧客户端安全缺省的 `mode`：`normal`、`start_goal`、
  `resume_goal`。缺失字段按 `normal` 读取，普通提交序列化时可省略；Desktop 现有 Composer
  显式提交 `normal`，`/goal` 交互仍归 M6。
- `start_goal` 只表达“以本次完整用户输入首次建立 Goal”的用户意图，不携带 GoalId、generation、
  turn、预算、Injected Part 或内部 Input 来源；这些事实均由 Runtime 分配和冻结。
- M3 稳定 `goal_already_exists` 与 `goal_unsupported_by_model` 错误，分别表达 Session 已有 Goal
  和当前冻结模型不支持 Tool Call。`resume_goal` 的跨层状态与命令闭环仍归 M4/M5，M3 Runtime
  对提前调用返回受控请求错误。

## v0.18.0 M4 Goal 生命周期协议边界

- `SubmitInputMode::ResumeGoal` 已由 Runtime library 实现为“以本次完整可见 UserMessage 恢复已有
  Paused Goal”的意图；客户端仍不提交 GoalId、generation、turn、预算或内部 continuation 内容。
- Stop、无消息 Resume、held Input Resume、Clear、Goal 快照、预算投影和事件在 M4 仍是 Runtime/Store
  私有能力，尚未增加公共 HTTP Command/SSE DTO。M5 必须基于 Runtime 权威结果统一稳定这些跨层契约，
  Desktop 不得先读取 Store 私有类型或根据 Conversation/Tool Result 猜测 Goal 状态。

## v0.18.0 M5 WorkPlan/Goal 应用契约

- `SessionViewSnapshot` 正式增加可选 WorkPlan/Goal，并在 Composer capability 中声明当前冻结模型
  是否支持 Goal；Queue item 只增加 `held_by_goal` 展示事实。旧 JSON 缺少这些字段时按无状态或
  不支持读取，Desktop 不复制 Runtime 私有类型。
- Goal 快照只包含 GoalId、generation、turn、状态、结构化暂停原因、预算、objective message ID、
  有界正文预览、附件数和时间；内部 objective payload/hash、Injected Part 和 Input origin 不进入
  产品协议。WorkPlan 快照保留 revision、objective、有序 item 与稳定 TodoItemId。
- ClearWorkPlan 以 expected revision 做 CAS；Stop/Resume/Clear Goal 均携带 GoalId 和 expected
  generation。Resume 可省略 InputId 以创建隐藏 continuation，也可指定 held Input；所有结果返回
  Runtime 权威快照/Run，而不是让客户端预测状态转换。
- `WorkPlanChanged`/`GoalChanged` 只作为 Session 快照失效信号；事件本身不携带完整业务正文。
  `goal_not_found`、`goal_generation_conflict`、`goal_not_resumable`、`goal_run_requires_resume` 与
  `work_plan_revision_conflict` 是跨 HTTP/持久 Run 投影稳定的错误码。

## Harness 验证

- 序列化 round-trip。
- 关键事件 JSON 快照或明确兼容性测试。
- ID、状态转换和可选字段语义测试。

```bash
cargo test -p assistant-protocol
cargo clippy -p assistant-protocol --all-targets --all-features -- -D warnings
```
