# assistant-runtime 模块约束

## 模块定位

`crates/assistant-runtime` 是应用业务运行时和权威状态所有者。正式产品由独立 Runtime Host
进程承载本 crate；Tauri 只通过应用协议连接，不直接嵌入或装配 Runtime。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- Session、Message、Run 的生命周期和查询。
- 多会话并发、同会话串行、全局模型/工具限流。
- Agent Core 实例管理和事件持久化/广播。
- 为完整 Trace 关联 Session、Run、AgentExecution、逻辑模型调用和 attempt，并负责录制模式、
  持久化、访问权限、保留、删除和 Complete/Incomplete 状态。
- 将配置、Agent 模板、用户偏好和会话覆盖编译成唯一不可变的 `ExecutionSpec`。
- 为每个 Runtime Run 创建绑定 `RunId`/`SessionId` 的 Recorder、Authorizer 和 Event Adapter。
- 在新建 Session 时读取 Pinned Memory Store、渲染并持久化冻结 System Prompt Snapshot；
  恢复、继续、压缩续接和分支复用原快照。
- 按应用配置装配 `PinnedMemoryStore`、检索用 `MemoryRecall`、可选续读用
  `RecallReferenceReader` 与具体 Recall Source，并通过普通工具注册给 Core。
- 定时任务、配置加载、后台任务与恢复。
- 装配真实文件/Shell Adapter，并负责授权、确认、环境策略与中断恢复。
- Repository/持久化边界和应用级错误。

## 并发模型

```text
Runtime Host 进程
└── Tokio Runtime
    ├── Session A：Run 串行
    ├── Session B：Run 串行
    ├── Scheduler
    └── Tool/Shell tasks（全局限流）
```

- 不同 Session 可以并发；同一 Session 默认只有一个改变上下文的 Run。
- 历史 Session 不绑定永久线程或永久任务；有 Run/定时事件时才激活。
- 每个 Run 有 `RunId`、状态、取消令牌和最终落盘结果。
- `RunId` 不传入 Agent Core；Runtime 将独立 `AgentExecution` 的输出关联回对应 Run。
- 客户端窗口销毁或事件订阅断开不得自动取消 Run。
- Runtime 必须显式持有每个 Run supervisor 的任务句柄。受控关闭先停止接收新工作并传播取消，
  只在配置的等待上限内等待；超时后终止仍未退出的 supervisor，并把仍处于活动态的 Run
  结算为脱敏的内部失败，不能让 Host 无期等待。
- 广播事件前后要有明确持久化策略，保证 UI 重建后能以快照恢复；不能依赖 WebView 永久在线。
- 模型、工具、Shell 和阻塞任务分别设置并发上限。
- Full Trace Collector 与普通 UI/调试广播分离；记录失败只标记 Incomplete，不得改变 Run 的
  模型或 Agent 结果，Conversation Journal 失败仍按权威业务错误处理。

## 文件能力

- Runtime 装配本地 `FileSystemTool` Adapter，负责工作目录、能力范围和确认；
  Adapter 负责真实路径解析、符号链接操作、大小限制和 I/O。
- `search` 可以调用打包的 `rg` 或 Rust 搜索后端，必须使用参数数组启动程序，不通过 `sh -c` 拼接查询。
- 写入优先采用同目录临时文件和原子替换；覆盖、删除等策略由权限模式决定。
- 文件授权只约束结构化文件工具，不能宣称约束 Shell。

## Shell 能力

- Shell 是通用工具，不维护不可持续的完整命令白名单。
- Runtime 装配本地 Shell Adapter，并注入工作目录、环境过滤与权限模式；
  Adapter 负责进程启动、输出、超时、取消和进程树清理。
- 支持 `Disabled`、逐次确认、会话信任、工作区信任和完全信任等策略；工作区信任不是 OS 沙盒。
- 正式对话保留 Shell Tool Call 和 Tool Result；不额外建立永久 Shell 审计副本。
- stdout/stderr 流式输出并限制总量；支持超时、取消和进程树清理。
- 默认不向子进程传递模型 API Key、应用令牌等敏感环境变量。
- 风险分析只提供提示，不能作为强安全边界。

## 配置与持久化

- 配置来源、默认值、覆盖顺序和校验必须显式。
- 配置合并结果只生成一份 `ExecutionSpec`；Core 不再二次解释协议适配、默认值或覆盖关系。
- 会话、消息、Run、定时任务和工具中断恢复事实由 Repository trait 隔离具体存储。
- Conversation Journal 是规范对话的权威状态；流式 `AgentEvent` 不是消息恢复来源。
- Runtime 提供绑定当前 Run 的 `ExecutionRecorder`，但其接口不向 Core 暴露 `RunId`、事务或数据库实体。
- 数据库写入错误不能伪装成成功事件；需要定义运行结果与持久化失败的处理策略。
- 数据库 schema 变更遵循根 `AGENTS.md` 的数据库安全规则，并提供迁移与回滚说明。

## 不应放在本模块的内容

- WebView DOM 状态、窗口位置、悬浮球视觉状态。
- Tauri command 宏和具体窗口句柄。
- 具体 UI 文案和确认弹窗实现。
- 与单次 Agent Loop 内部算法强相关但不涉及应用调度的逻辑。
- Provider 消息编解码、规范工具分发、上下文压缩算法和启发式 Guardrail 实现。

## v0.4.0 设计边界

- v0.4.0 不修改 `assistant-runtime` 代码、依赖、Session/Run 类型或产品协议。
- Core 只提供 resolved invocation、Allow/Deny 策略装配与 Guardrail；Plan/Build、
  Ask/Auto、规则保存、审批交互和恢复仍属于 Runtime 的上层编排职责。
- 真实文件与 Shell 机制由独立本地基础设施 Adapter 提供，Runtime 负责注入
  工作目录、能力策略、环境过滤、审批和中断恢复；Adapter 不反向定义 Runtime API。
- `tools/safety-demo` 在 v0.4.0 中临时验证上述行为，其 Session、Run、审批、HTTP 和
  内存审计类型均不是正式 Runtime 契约，不得直接复制到本 crate。
- v0.4.0 的本地 Adapter 不承诺事务或原子文件替换；正式产品接入若需要更强写入
  保障，应在后续 Runtime 总体设计中明确能力要求和恢复语义。

## v0.5.0 设计边界

- v0.5.0 不修改 `assistant-runtime` 代码、依赖、Session/Run 类型或产品协议。
- `agent-memory` 只提供实现无关的 Pinned Memory 与 RecallSource 契约；标准记忆工具壳
  通过普通 Tool Loop 工作，Core 不增加 Memory 专用阶段。
- 未来 Runtime 负责选择 Store/RecallSource Adapter、编译 Source 可见性和授权规则、
  创建冻结 Session Prompt Snapshot，并持久化其成品状态。
- `tools/memory-demo` 在 v0.5.0 中私有验证 JSON Store、RecallSource、Session、Journal
  和 CLI 行为，这些类型、文件格式和命令不得直接复制为正式 Runtime 契约。

## v0.6.0 设计边界

- v0.6.0 不修改 `assistant-runtime` 代码、依赖、Session/Run 类型或产品协议。
- Model/Provider 只增加结构化错误、attempt、wire 观察和有限建立重试；正式 Runtime 未来负责
  是否装配、如何关联和持久化，不把 Trace 生命周期下推给 Core 或 Adapter。
- `tools/reliability-demo` 私有验证完整 Trace、JSONL、Complete/Incomplete、Wire/Model Replay
  和 Timeline；这些文件、metadata、宿主事件和 CLI 不得直接复制为正式 Runtime 契约。
- 流建立后失败、Context Overflow、Length 或任务未完成后的续跑仍需未来 Runtime 显式启动新的
  AgentExecution，不与本版本的同一步建立重试混同。

## v0.8.0 初始化边界

- 正式 Runtime 由 `apps/runtime-host` 独立产品进程装配，不再直接嵌入 Tauri 进程。
- `v0.8.0` 只实现内存 Session、Conversation、Run、事件、取消和受控关闭；不实现 Repository、
  Scheduler、Plan/Build、审批、Workspace、记忆或系统后台托管。
- 同一 Session 活动 Run 未结束时返回 `SessionBusy`，不隐藏排队；不同 Session 使用 Tokio 任务
  并发，不绑定永久线程或永久 actor task。
- Runtime 事件允许丢失，Conversation 与 Run 快照是查询依据；本版本不提供事件回放。
- Runtime Host 的 Unix Socket、私有 wire、Demo 和具体 Provider/工具装配不得进入本 crate。

## v0.9.0 配置与模型装配边界

- Runtime 持有唯一配置源与当前有效快照；Host 只提供 Runtime Home 文件读取和具体 Provider
  构造能力。配置缺失或顶层无效时保持可诊断但 fail-closed，不回退旧 credential。
- Session 冻结用户 model key 和渲染完成的 System Prompt；每个 Run 从同一份配置快照编译
  ModelService、请求限制和 Agent。reload 只影响之后开始的 Run，不改变活动 Run。
- 连接验证复用 Run 的模型编译链，但不创建 Session、RunRecord、Conversation 或 Journal；
  对外只返回稳定的脱敏结果分类。
- Runtime 关闭等待上限属于进程装配参数，不属于模型 TOML。当前实现超时后中止 Runtime
  supervisor 并强制结算业务 Run；Core 内部执行任务由 execution-owned observer 独立观察并将
  panic/JoinError 收敛为稳定内部失败，不能用 Runtime 兜底替代 Core 自己的任务所有权。

## v0.10.0 持久化边界

- `assistant-runtime` 定义按业务原子操作表达的 `RuntimeStore` 端口及经过校验的存储 DTO；本 crate
  不依赖 SQLite、Runtime Home 路径、文件句柄或具体 JSONL 提交算法。
- SQLite 结构化状态和每 Session Conversation JSONL 共同组成产品数据集，但规范 Conversation
  仍是正文事实源；流式 RuntimeEvent、pending exchange 和 staged append 不进入规范快照。
- 产品 Conversation 投影、Around 定位、Markdown 导出和 Conversation Recall 必须排除
  `TranscriptVisibility::Hidden` 的 Runtime User Message；规范快照、Context Layout、重放、Fork
  和物理 `message_count` 仍保留它，不能把产品可见数冒充正文物理数量。
- Runtime 通过异步 `open` 在进入 Running 前恢复 Session 与 Run registry；Conversation 只在首次
  查询或执行时读取并缓存，单 Session 正文不可用不得回退成第二份空状态。
- 创建 Session、提交 User Message 与 Run 起始、结算 Run 终态均先完成 Store，再更新内存投影和
  发布事实事件；`message_count` 是列表派生投影，不替代正文物理顺序。
- 输入提交先由 Store 原子创建持久 Input 与首次 `Accepted` Run，再更新内存投影、发布事件并唤醒
  Session 队列；同一 Session 的幂等 key 只返回首次 Input/Run，不比较重复请求正文。
- queue order 只属于结构化输入状态，不进入 Message 正文。每个 Session 至多存在一个队列执行器，
  同 Session 按接收顺序领取，不同 Session 可并发。
- 启动恢复时，尚未开始的 queued Input 保持 `Accepted` 并令 Session 进入 `resume_required`，不自动
  调用 Provider；已提交 User Message 但未可靠终结的 Run 转为 `Interrupted`。`ResumeSession` 恢复
  整个队列，`RetryRun` 只放行目标 Input 的新 attempt，不隐式恢复其后的排队输入。
- M3 由绑定当前 Run 的 Runtime Recorder 在工具副作用前持久化 begun，结果齐备后先转 ready，
  再把 Assistant Tool Call 与全部 Tool Result 整批提交正文并清除 pending；任一 Store 失败均令
  Session fail-closed，不允许后续结算越过未修复的工具交换。
- Runtime 自己的队列/supervisor task 由 RuntimeTasks 观察 panic，并执行 Run 失败收敛；强制关闭
  遇到 pending 时保留持久恢复事实，不把未知副作用伪装成普通 Failed。queue/supervisor panic
  后即使当前 Run 已能结算，Session 也保持 fail-closed，不在旧 Core 执行退出尚未被 Runtime
  重新观察时唤醒下一条输入；下次启动从持久事实恢复。
- Runtime 关闭分别限制 supervisor 等待、强制结算和 Store shutdown；强制结算失败仍要尝试
  flush/join Store，任何阶段超时都返回可观察错误，不能让 Host 无限等待。
- M4 将 Session 的 model key 纳入 mutation gate 保护的可变状态，System Prompt 仍从创建时冻结。
  只有 active 且完全空闲、无 queued Input 或未终结 Run 的 Session 才能归档或切换模型；归档
  只改变生命周期并保持所有只读事实，恢复不会自动启动工作。
- 历史重新输入只定位规范 User Message：先校验新正文并构造 Agent，再由 Store 原子切换完整新
  generation、删除目标及尾段 Input/Run/引用、创建全新 committed Input 与 Accepted Run。提交后
  Runtime 同步替换 Journal 和结构化投影，不保留隐式分支或 undo；同一幂等 key 仍返回首次结果。
- `ListRuns` 按持久 Input 接收顺序和 attempt 返回快照；正文 generation、queue order 和物理删除
  细节不进入 Runtime 公共协议。
- 正式本地 Adapter 由 Runtime Host 的单一阻塞 worker 持有；无 Host 的 Runtime 单元测试可使用
  crate 内易失 Store，但正式产品入口不得绕过本地 Adapter。

## v0.11.0 Workspace 与附件边界

- M1 由 Runtime 持有按 ID 排序的 Workspace Registry，并通过 `RuntimeStore` 业务端口完成
  登记和假删持久化；查询与活动列表读取 Runtime Registry。canonical path、SQLite 与 Runtime
  Home 物理布局只由 Host Adapter 处理。重新登记同一 canonical path 恢复原 Workspace ID。
- Session 创建可以选择一个 active Workspace；Runtime 在创建事务前通过
  `SessionEnvironmentFactory` 一次性生成 `SessionExecutionEnvironment` 和
  `SystemPromptSnapshot`。Session Controller 持有该冻结环境，不提供换绑操作；Workspace
  假删只阻止新 Session 绑定，既有 Session 保留原绑定。
- Workspace 私有目录和 Session 私有目录统一属于 Runtime 管理的 Agent 私有目录，但必须保留
  作用域区分；Session 附件目录承载静态参考文件，不纳入 Agent 私有目录口径。
- Runtime 持有 Workspace 登记、Session 冻结绑定、Attachment 准入/提交、File References
  Part 构造和附件内容去重语义。Host 的 HTTP router、multipart parser、SSE、Bearer Token
  和发现文件不进入本 crate。
- 附件上传拆成“流式前准入”和“staging 完成后提交”两步 Runtime 业务操作；
  Host 可以持有短期上传资源，但不得复制 Attachment 状态机。
- M2 由 Runtime 持有 Attachment Registry，查询与列表不机械下沉到阻塞 Store；Store 只承担
  上传原子提交和启动恢复。上传只允许 active Session，同一 Session 相同 Blob Hash（同名同内容）返回首次事实。
- M3 在 Input 持久化前把有序 Attachment ID 整批解析为原始名称与稳定路径，
  并作为独立 `UserPart::FileReferences` 进入 queued JSON 和 Conversation JSONL。
- Core 在完整 Model Step 结束后报告的 token usage 由 Runtime 投影为只读实时事件；Runtime
  不重新估算 Provider total，也不为 Demo 增加独立持久字段。事件断线恢复继续以规范
  Assistant Message 中已持久化的 usage 为准。
  重复 ID、跨 Session 引用和 unavailable Attachment 在入库前拒绝；输入幂等命中早于附件重新解析。
- Runtime 为每个 Run 装配模型 attempt observer，并投影脱敏的 attempt/retry 事件。模型失败
  结算区分建流前与建流后，记录稳定分类、实际 attempt/retry 数和是否已有可见输出；Provider
  原始错误文本不进入公共事件、Run 错误或普通日志。最终摘要复用现有 Run 错误持久化字段。
- 每个 Run 在模型 Agent 构造前，通过 `RunToolFactory` 根据冻结的 Session Environment
  同时编译 `RunToolBundle` 中的 ToolSet 和 Host 基础设施策略；Runtime 再把后者与
  Run 冻结的变体、审批方式及动态权限快照组合为唯一 Authorizer。ToolSet 在该 Run 内保持不变，
  不进入 Protocol、Conversation 或持久化正文。不同 Workspace 不得共用一个可变全局
  `SessionPathResolver`。
- Runtime 只定义按 Run 装配端口和脱敏错误映射，不直接依赖 `agent-tools-local`。
  绑定 Workspace 的工作目录在 Run 前不可用时返回 `WorkspaceUnavailable`；其他工具构造
  失败按 Agent 构造失败处理，不向客户端泄露路径或底层 Adapter 错误。
- Workspace 假删只阻止新 Session 绑定；已绑定 Session 的冻结路径和持久事实不改写。
  `ApplicationSnapshot` 与 `list_workspaces` 只投影活动 Workspace，并从活动/归档 Session 列表隐藏
  绑定已移除 Workspace 的 Session；重新登记同一 canonical path 恢复原 Workspace ID 后，
  这些 Session 自然重新进入组合快照。
  附件正文不自动进入模型上下文，Agent 通过持久化 File References 和文件工具按需读取。

## v0.12.0 Agent 变体与权限边界

- Plan/Build 使用同一份 Tool Definition；每个 Input 显式携带变体，Runtime 将对应注入作为
  `UserPart::Injected` 持久化。Session 当前变体只服务于客户端重建，不覆盖 Input 实际提交值。
- 每个 Run 冻结 Input 变体与当时的 Ask/Auto，但每次 Tool Call 都读取 Global、可选 Workspace、
  Session 三层 Permission Registry 最新快照。显式 Deny 优先于 Ask，Ask 优先于 Allow；未知
  typed facts 和无效权限快照一律 fail-closed。
- Workspace 的产品默认信任通过 Host 生成的普通 Workspace 权限文档表达，不进入 Authorizer
  硬编码：Plan/Build 可读取，Build 可变更 Workspace。Runtime 注册 Workspace scope 时加载 Store
  中的实际文档；既有用户文档、无效文档和缺失文档的诊断语义不得被默认值静默覆盖。
- Session 的产品默认信任使用同一装配策略：Host 在 Session 创建、Fork 和旧数据恢复时，仅在
  `private/permissions.json` 缺失时生成普通 Session 权限文档。Plan/Build 可读取 Session 私有目录
  与该 Session 的附件目录，Build 可变更 Session 私有目录；附件 mutation 继续由 Host 基础设施
  策略拒绝。Runtime 创建或 Fork Session 后必须加载 Store 中的实际文档，不得先注册空 scope 或
  在 Authorizer 中硬编码放行。既有 Session 权限文件不得覆盖或合并，权限文件本身不得被结构化
  文件工具修改。
- Plan 对结构化文件 mutation 施加不可由规则或审批覆盖的能力上限：只允许进入 Session 和可选
  Workspace Agent 私有目录的普通权限决策，并对现有路径祖先做物理解析以拒绝 symlink 逃逸。
  Shell 仍是当前用户权限下的通用工具，不能被表述为 OS 沙箱。
- Ask 由 Runtime 内存 `ApprovalRegistry` 挂起 Tool Call；公共命令可按 Session 查询 pending，
  并原子提交 AllowOnce、AllowSession、可用时的 AllowWorkspace 或 Deny。客户端断线不取消审批，
  Run 取消、关闭或等待 Future 被丢弃必须移除审批；pending 不入库，重启不恢复。
- Session/Workspace 持久允许必须先完整读取并校验 JSON、写入 exact Allow、完成 revision CAS、
  更新 Registry，才唤醒 Core。首次 revision 冲突只重载并重试一次；写入失败保持 pending，绝不
  执行工具。交互审批不提供 Global 写入口。
- 审批返回 Allow 前再次执行 Plan 硬边界、Host infrastructure Deny 和当前显式 Deny；旧审批不能
  覆盖等待期间新增的拒绝规则。审批等待不持有 Session mutation lock，其他 Session 和当前
  Session 的只读查询保持可用。
- Core 只新增实现无关的 `mark_tool_execution_started(receipt, call_id)` 可靠接缝；Runtime Recorder
  在实际 Tool SPI 前调用 Store，失败时不执行工具并形成错误 Tool Result。工具输出正文不复制到
  其他投影，完整结果仍由 Conversation Tool Message 保存。

## v0.13.0 单层子任务执行边界

- 父 Run 在冻结 ToolSet 上稳定追加 `delegate_task`，子 Agent 只复用原始 Base ToolSet，不能再次
  委派。父子复用冻结模型、Context、变体、权限作用域与 Workspace/Session 持久目录；子任务另持有
  一个只服务本次执行的 OS 临时目录 lease，终态可靠提交后释放。
- `delegate_task` 是显式 `ParallelEligible` 工具；同一模型 Turn 中连续的委派可由 Core 并发执行，
  但每个父 Run 仍通过 Runtime Semaphore 限制实际运行数。等待 permit 不创建临时目录、不持有
  Runtime 长锁，并可被父级或单独取消唤醒。
- 每个父 Run 冻结子任务总数、并发数以及子 Agent 的 step、tool、output 和执行超时上限。子 Agent
  的 step/tool/output 同时受全局 Agent 配置约束，实际取二者较小值；超时从取得执行 permit 后开始。
- 父子执行发生自动压缩时，continuation 从 Core 的可靠 CompactionRequired 终态扣减已消费 Step 与
  实际 dispatch 工具数，不得用可丢弃的事件流计算剩余预算。整体摘要活动 Turn 后，replacement
  必须保留一条持久化的 Injected User continuation 锚点，保证后续 Assistant 仍属于合法 User Turn。
- 每个活动 child 拥有派生自父 Run 的取消令牌。父 Run 取消和 Runtime shutdown 级联到全部 child；
  单独取消只影响目标 child。可靠完成与取消竞争时允许已经形成的完整终态获胜，timeout 则稳定结算为
  `failed/timeout`，不能伪装成成功。
- child 工具审批复用 Session/Workspace 权限 Registry，但审批快照携带可选 `child_task_id`；一个 child
  等待审批不占用 sibling 的权限锁，也不阻止 sibling 执行。取消 child 必须移除其 pending approval，
  且不得执行待审批工具。
- child usage 只随其独立规范 Assistant Message 持久化；父 Run 不保存聚合副本，客户端按查询结果
  动态汇总。M4 已接入正式 list/get/cancel、生命周期与消息事件：查询严格核对 Session/父 Run
  所有权，取消先写 Store 的 `cancel_requested` 再发布活动 token，终态重复取消不改写可靠结果。
- child 事件是从独立 `AgentExecution` 事件流投影的观察事实，envelope 始终携带 Session、父 Run
  和 child ID；事件丢失后以 Store 重建的 Snapshot/Conversation 为权威，不在 Runtime 保留第二份
  持久事件日志。
- pending tool start 只在 exchange 生命周期内持久；Tool Exchange 完成时随 pending 记录
  级联清理。启动恢复使用它区分“未开始”和“已开始但结果未知”，不重新执行工具。
- 本版本不建立永久工具审计投影、facts JSON 或公开审计查询；持久权限看 JSON
  规则，正式工具历史看 Conversation，pending approval 看 Runtime 实时投影。

### 存储与 Recorder 复用约束

- `RuntimeStore` 用明确的 child 业务操作承载关系创建、初始消息、工具交换、终态结算和正文读取，
  不暴露通用 SQL、路径或 JSONL offset；易失 Store 与正式 Host Store 必须保持同一状态约束。
- 子任务拥有独立 Journal 和 mutation gate。`RuntimeRecorder` 的 begin/started/complete 算法只有
  一份，私有 `RecorderTarget` 只适配父 Run 与 child 的所有权、Store DTO 和内存提交目标；
  Core `ExecutionRecorder` 契约不增加 Runtime 业务类型。
- `delegate_task` 作为普通 Runtime 工具接入父 Run。父工具集由 Host Base ToolSet 追加
  委派工具派生；子 Agent 只使用 Base ToolSet，不能递归委派，也不读取父 Conversation。
- 子 Agent 复用冻结模型、请求配置、Context、变体、Guardrail、权限作用域和现有 AgentExecution，
  使用独立 Recorder/Journal。子终态必须先可靠提交，父 Tool Result 才能返回最终文本或受控错误。
- OS 临时目录通过 `ChildTaskWorkspaceFactory` 端口由 Host 创建并以 lease 管理；Runtime 不直接
  操作文件系统。并发、timeout、独立取消和公共查询均由 Runtime 在同一 child 生命周期上编排。

## v0.16.0—v0.17.0 模型协议与能力编译边界

- `protocol` 规范值为 `openai_chat_completions` 或 `openai_responses`；读取旧
  `chat_completions` 时只在内存中归一化，新的模型配置写入规范值。Responses 不接受简称或
  Chat 别名，未知协议在模型配置编译期失败。
- Runtime 接收 Host 已严格校验的只读 `ModelCatalog`，并按
  `(provider, protocol, model_id)` 精确匹配；目录未命中时使用当前协议的保守能力基线。
- 用户 capability override 优先于目录；reasoning 块只允许整块替换。effort 只接受五个稳定 key、
  非空 label 和字符串或正整数 wire value，不接受字段名、JSON path 或请求片段。
- `ResolvedModelConfig` 同时持有 Route、Protocol 和唯一 `ResolvedModelCapabilities`；Core、Host
  与连接验证不得再次按模型名前缀猜测能力。

## v0.17.0 Tool Result 图片资源边界

- `SessionExecutionEnvironment` 持有 Host 从 Session ID 确定性构造的
  `session_tool_image_directory`；该路径不写入 System Prompt、Conversation 或数据库列，
  恢复时必须从固定 Session 目录重新构造。
- Runtime 的 Conversation、pending exchange、staged append 和 child Conversation 继续通过
  `agent-types` 的统一 Serde 保存有序 Tool Result Parts；Runtime 不建立图片表、引用计数、
  全局缓存或第二份图片正文。
- Fork 请求只携带从所 Fork Conversation 收集并去重的 `ToolImageReference`；Host Store 在
  提交新 Session 事实前完成独立字节复制，失败时回滚新 Session 资源。不同 Session 不共享
  inode、链接或生命周期。
- Session 删除继续删除整个 Session 私有目录，因此 `tool-images/` 与 Session 共生共死；
  Runtime 不实现跨 Session 垃圾回收。
- Runtime 从精确 `(provider, protocol, model_id)` 能力编译得到 `ToolImageProjection` 和
  `ToolChoiceCapabilities`。目录 schema v2 对不自洽组合 fail-closed；未命中路由保持
  `Unsupported`/Auto-only 保守基线。
- 只有主模型同时具备 `image_input`、Tool Call 和非 `Unsupported` 投影时，Runtime 才要求 Host
  注册 `read_image`。文本主模型仍只在有效辅助视觉模型存在时注册 `inspect_images`，两条路径
  不互相调用。
- `inspect_images` 与 `read_image` 统一服从文件 Read 权限。多图事实按路径分别合并分层规则：
  任一 Deny 拒绝整次调用，任一 Ask 进入审批，全部路径被 Allow 才直接执行；交互式持久批准把
  每条 exact Read 规则作为一次权限文件 CAS 原子写入，不能折叠为共同父目录。
- 图片准备包装器在主/child Run 和辅助识图调用构造时绑定当前 Session Environment；连接验证
  不绑定会话资源。它按附件/Tool Image 来源身份去重预处理，不在 Provider 请求前另设全局图片
  数量上限；具体模型或服务商拒绝请求时沿既有模型错误链路返回。
- `openai_responses` 与 Chat 一样使用本地 Conversation 作为唯一历史事实；Runtime 不保存
  `previous_response_id` 或服务端 Conversation ID。目录未命中的 Responses 路由采用本协议的
  保守能力基线，不能因 endpoint 或模型名称自动启用图片、reasoning 或工具选择能力。
- Runtime 持久化规范 ReasoningPart 与 Adapter 返回的 OpaqueProviderState，但不解释、不解密、
  不摘要 payload，也不建立独立缓存表。旧 opaque state 可随普通压缩 head 被 replacement 替代；
  最近 Turn、活动 Turn 和完整 Tool Exchange 的保护继续由 `agent-context` 统一决定。
- 工具详情中的 Session Tool Image 必须从对应可靠 Tool Result Image Part 生成稳定引用；
  `read_image` 的 Tool Call `path` 只保留审计语义，不能被投影成图片产物或用于预览回源。
- 资源解析按 owner/message/call/part 回查可靠 Conversation。主会话与 child Conversation 使用同一
  Image Part 算法；Tool Image 显式排除出 `project_conversation_file_references`。

## v0.18.0 M1 Session WorkPlan 边界

- WorkPlan 是独立于 Goal 的 Session 权威状态，包含 revision、总体 objective、有序扁平 item 与
  更新时间；最多 100 项、ID 唯一且同时最多一个 `InProgress`。它不是 Goal 子对象，也不决定是否续跑。
- 父 Agent 在支持 Tool Call 时由 Runtime 追加私有 `update_plan` 工具；child 只持有 Host Base
  ToolSet，不能改写父 Session 计划。该工具使用 Runtime 私有授权 facts 自动允许，不进入 Ask/Auto，
  但 Tool Call/Result 仍按普通可靠 Tool Exchange 保存。
- `update_plan` 使用完整替换语义，但 `TodoItemId` 只属于 Runtime 持久化与产品投影，不进入模型
  输入、隐藏上下文或工具结果。Runtime 在内部按文本、原位置和剩余项依次调和 ID，新项才分配；
  模型侧每次必须提交完整 objective；只更新事项时原样重复当前 objective。旧 Conversation 仍兼容
  缺省 objective，并按兼容字段接受及忽略模型提交的 `id`。Runtime 以当前 revision 做 CAS，并以模型
  ToolCallId 作为 Store operation 幂等身份；Store 成功后才替换 Session 内存投影，真实 revision
  conflict 不做容错覆盖。
- WorkPlan 通过 `RuntimeStore` 的 load/mutate/clear 业务操作持久化。易失 Store 与正式 Store 必须保持
  相同的 revision、幂等、归档、Fork 和删除约束；非法恢复内容令 Runtime 启动 fail-closed。
- `update_plan` 提交至少一个且全部 Completed 的 item 时，Store 必须在同一事务保存 ToolCallId
  完成回执并删除当前 WorkPlan；重放返回首次计划结果，SessionView 与后续 claim 均视为无计划。
  空 item 的总体目标计划不触发自动清除。
- 排队输入不提前冻结计划。queue driver 在实际领取一条新用户输入、首次提交 Journal 前，把当时最新
  WorkPlan 追加为版本化 `WORK_PLAN_CONTEXT_V1` Injected Part；已进入 Conversation 的历史消息不回写，
  retry 继续沿用已有历史上下文。
- Fork 在 Session 创建操作中把源 WorkPlan 复制成目标 Session revision 1 独立快照；归档保留计划，
  Session 永久删除随 Store 级联清理。M1 不增加 Goal、自动 continuation 或产品 WorkPlan 查询事件，
  后者属于 v0.18.0 后续协议里程碑。

## v0.18.0 M2 Goal 领域与 Run 信号边界

- Goal 是独立于 WorkPlan 的 Session 权威状态，主状态仅为 Running、Paused(reason)、Completed；
  清除表示控制器不存在。冻结预算固定为 20 Run、500,000 total tokens、连续失败 3 次，恢复值必须
  满足版本常量和状态/时间不变量。
- objective 恢复副本只保存原始消息 ID、Text/FileReferences 有序用户载荷和
  `sha256-v1` 内容哈希；恢复时重新序列化并核验哈希，Injected Part、摘要或 LLM 重述都不能成为来源。
- 所有支持 Tool Call 的父 Agent 都获得 Runtime 私有 `update_goal`；child 不获得。普通 Run 调用得到
  受控错误，不改变 Goal。Goal-bound Run 必须以 GoalId、generation、RunId 三元绑定创建唯一内存 latch。
- `update_goal` 必须是 Assistant Turn 唯一 Tool Call；混合或重复信号批次全部拒绝。latch 接受信号后，
  当前 Run 的后续工具全部拒绝，只允许模型生成最终正文。工具只记录 complete/blocked 与 summary，
  不直接写 Store 或切换 Session Goal。
- latch 由 Run 装配层持有并随执行对象释放，不进入 SQLite、Conversation、Recorder 或协议 DTO。M2 不把
  latch 应用到 Run 结算，因此模型流、Recorder、取消或进程失败不会提前转换 Goal；可靠结算转换与
  continuation 属于 M4。
- Runtime 恢复把 Store 已暂停的 Goal 经领域校验装入 Session；孤儿、重复 Session Goal、错误 hash、
  非法预算或状态/暂停原因组合均令恢复 fail-closed。M2 不实现 `/goal` 输入、Goal Input binding 或自动续跑。

## v0.18.0 M3 Goal Input 与首次启动边界

- `StoredInput` 以正交的 `InputOrigin::{User, Runtime}` 和可选 Goal binding 保存来源、GoalId、
  generation 与 turn。User Input 必须对应可见的 User-origin Message，Runtime continuation 必须
  绑定 Goal、保持 transcript hidden 且只包含 Injected Part；非法恢复组合令启动 fail-closed。
- `start_goal` 在 Runtime 内构造一条仍属于 Conversation 的可见 User Message，并追加版本化
  Goal 指令。objective 恢复副本只从该消息的 Text/FileReferences 产生，附件顺序与正文完整保留，
  Injected Part 不进入 objective。
- Store 的高层 Input 接受操作必须把新 Goal、Goal-bound Input 和首次 Run 原子提交；同一
  idempotency key 返回首次事实，不能重复建立 Goal。模型不支持 Tool Call 或 Session 已有 Goal 时
  在持久化前拒绝。
- Session 调度把普通用户输入与 Goal 输入放入独立 lane。存在 Running Goal 时只领取匹配当前
  GoalId/generation 的 Goal lane，之后到达的普通用户输入只持久排队，不会被 Goal 自动消费；
  Queue 的优先级和取消只作用于普通用户 lane。
- M3 只建立首次 Run，尚不消费 `update_goal` latch 来结算 Goal，也不创建 Runtime continuation。
  Stop/Resume、预算结算、重启恢复、Fork 和 Goal 历史生命周期统一归 M4；为避免破坏 objective，
  M3 已禁止 Goal 存在时执行历史重入。
- 物理模块按领域与用例分层：`src/goal/` 只保存 GoalControl、objective、上下文与 signal；
  `runtime/goal/commands.rs` 负责 start 准入、resume 未开放门禁及 Goal 专属事实构造；
  `runtime/input/submission.rs` 负责普通与 Goal 输入共享的消息准备、原子接受和 lane 投影。
  后续 M4 的结算与 continuation 继续内聚到 `runtime/goal/`，不得回填进通用 Input commands。

## v0.18.0 M4 Goal 续跑与生命周期边界

- Run settlement 是 Run、Goal、预算和后继 Input/Run 的单一高层 Store 操作。Runtime 先根据本 Run
  signal、规范 usage 和执行结果构造 effect，Store 原子提交后再用返回的权威 Goal/continuation
  更新 Session 投影；不能先在内存切换 Goal 或另行入队后继。
- 没有 complete/blocked signal 且预算未触发时，结算创建只含版本化 Injected Part 的隐藏 Runtime
  continuation；complete 在结算事务中形成 Completed 回执并立即删除 Goal，blocked 转
  Paused(Blocked)，Run/token/连续失败预算统一转为 Paused(BudgetExceeded)。usage 不完整时
  不猜测 token，但仍记录该 Run 并保持 usage incomplete 事实。
- Stop 是 Goal 级操作：先持久暂停为 UserStopped、递增 generation、删除未领取 Runtime continuation，
  并为活动 Run 记录取消意图；成功后才发布取消 token。当前 Goal Run 不允许走普通单 Run cancel。
  Clear 主要删除已暂停且无活动 Run 的控制器；完成 Goal 已在结算事务中自动删除。两者都不删除
  Conversation、WorkPlan 或 held 用户队列。
- Resume 是显式的新 generation。它可创建无正文隐藏 continuation、原子接受新的可见用户消息，或把
  已有 held User Input/Run 绑定到新 generation；第三种方式复用原 ID，不复制队列项。所有方式均由
  Store 返回权威结果后更新 Session，迟到 signal、取消回调和旧 continuation 不能越过 generation。
- 启动恢复把 Running Goal 原子转为 Paused(RecoveryRequired)、递增 generation 并删除旧 queued
  continuation。Fork 仅在 objective source message 位于复制前缀内时复制 Goal，分配新 GoalId 并以
  Paused(Forked)、generation 1 建立独立控制器。历史重入到 objective 或之前拒绝；重入其后时随
  Conversation generation 切换原子暂停 Goal。Session 删除仍按 Session 所有权清理 Goal 和历史绑定。
- 恢复时只有仍为 Queued 的 Goal-bound Input 必须匹配当前 GoalId/generation；Committed Input 的
  binding 是历史审计事实，可以引用已完成、已清除或上一生命周期的 Goal，不能因此要求当前控制器存在。
- 领域实现继续内聚：`runtime/goal/settlement.rs` 负责 effect，Goal commands 负责 Stop/Resume/Clear，
  通用 Input 模块只提供消息准备与 lane 投影。M4 不增加 Desktop 快照、HTTP 命令或 SSE 事件；这些
  公共应用契约属于 M5。

## v0.18.0 M5 产品投影与控制命令边界

- SessionView 从 Session controller 的权威内存状态统一投影 WorkPlan、Goal、held Queue 与当前模型
  Goal capability；Goal objective 只派生有界预览和附件数，不返回恢复 payload/hash。Runtime 不从
  Conversation 文本、Todo 状态或 `update_goal` Tool Result 反推 Goal。
- ClearWorkPlan、Stop/Resume/Clear Goal 都先进入 Session mutation gate，核对 revision 或
  GoalId/generation，再调用高层 Store 操作；只有持久化成功才替换内存投影并发布失效事件。
  Goal-bound Run 的普通 retry/cancel 返回 `goal_run_requires_resume`，保持 Goal 级控制入口唯一。
- `update_plan` 在 Store CAS 与 Session 投影成功后发布 `WorkPlanChanged`；Goal 首次启动、用户消息
  resume、生命周期命令、Run settlement 和强制 shutdown settlement 在权威转换后发布
  `GoalChanged`。事件只要求客户端重新读取 SessionView，不成为第二份业务状态。

## Harness 验证

- 覆盖不同 Session 并发、同 Session 串行、取消、排队、订阅断开、重启恢复和 Scheduler 补跑策略。
- 文件和 Shell 测试使用临时目录与无破坏命令，不访问用户真实文件。

```bash
cargo test -p assistant-runtime
cargo clippy -p assistant-runtime --all-targets --all-features -- -D warnings
```

## v0.19.0 统一内部上下文边界

- `InternalBoundaryCoordinator` 是 Runtime 新建内部上下文 Part 和隐藏 UserMessage 的唯一构造入口；
  Goal、WorkPlan、Plan/Build 变体和委派只提供冻结正文、来源与 retention key，不自行分配 boundary。
- 新写入统一使用 `UserPart::InternalContext`；旧 `Injected` 继续安全读取、进入模型上下文并在产品
  转录、搜索、导出和 Goal objective 中排除，不批量重写历史。
- 规范内部消息继续由既有 Input、Goal settlement、Context replacement 和 child 事务可靠提交；
  request-only Provider 信封不进入 Store、Conversation、RuntimeEvent、Run step 或产品消息。
- 压缩时每个 retention key 的最新冻结正文必须可恢复；活动 Turn 整体摘要时由共享 Context 能力
  原样重挂到新的隐藏 continuation 锚点，Runtime 不从摘要文本猜测恢复。

## v0.19.0 Skill 发现与 Session Catalog 边界

- Runtime 定义通过格式校验的 `SkillName`、四级 `SkillSource`、只保留源目录的候选项、诊断、
  名称开关和文件实现无关 `SkillPackageSource`；Runtime 不解析 YAML、枚举用户 Home 或读取文件。
- 当前管理投影按“工作区 `.ez-assistant`、工作区 `.agents`、用户 `.ez-assistant`、用户
  `.agents`”固定顺序选 Winner；同来源同名冲突不任选，扫描不完整时不暴露部分 Winner。
- 名称开关只以 `SkillName` 为键；没有 Store 记录表示启用，禁用屏蔽全部同名候选且不回退下层。
  Skill 启用与 `allowed-tools` 均不形成 Authorizer 规则。
- 新 Session 创建时，Runtime 读取名称开关、扫描并编译 Winner；扫描整体不可用时冻结
  `unavailable` 空 Catalog，不把部分 Winner 当作可调用事实。
- `SessionSkillCatalog` 保存完整 `SKILL.md` 定义、definition digest、共享源目录、确定性 revision、
  状态和诊断，不保存普通资源索引或字节；模型目录作为独立 `SystemPromptSnapshot` Part 冻结，
  绝对源路径不进入模型文本或 revision。
- Catalog 随正式/易失 Session Store 持久化并由 `SessionController` 只读持有。Runtime 重启、归档恢复
  和后续执行只消费该快照；Fork 原样继承相同内容身份和共享源目录，不重扫、不改写路径或复制文件。
- 用户显式激活只接收一个 `skill_name`，并只查询目标 Session 的冻结 Catalog；当前 Root 文件和
  名称开关的变化不能替换已接受 Input。用户激活直接随 Input 落账，不额外执行 `load_skill`。
- 用户 Activation 与可见 UserMessage 的 `InternalContext` Part、queued Input、首次 Run 和 ledger
  由同一 Store 操作原子保存。Queue、Conversation 和 Active Skill 投影都读取结构化 Activation，
  不解析内部正文；取消、held Goal 恢复、历史重入和 Session 删除同步维护 ledger 所有权。
- Goal objective 在附加 Skill Part 前冻结，且只接收真实用户正文/附件；held Input 后补 Goal 上下文时，
  统一边界协调器把 Goal Part 插到 Skill Part 之前，不由功能模块直接拆装内部 Part。
- Fork 只复制 Conversation 前缀内的 Activation，分配新的 ledger 身份并改绑目标 Session；历史消息 ID、
  Catalog revision、definition digest 和触发来源保持不变，Input/Run 归属置空表示继承历史。
- `ListSkills` 每次显式调用都重新扫描当前管理范围；`SetSkillEnabled` 只按名称保存全局开关并发布失效
  事件，不刷新既有 Session Catalog 或 Activation。

## v0.19.0 模型 Skill 激活与同 Run continuation

- Runtime 对支持 Tool Call 的模型始终注册稳定 `load_skill`；`name` 是普通字符串且不生成动态 enum，
  空或不可用 Catalog 也由执行结果表达，不因扫描变化替换 ToolSet 定义。
- 工具只读取 Session 冻结 Catalog 并校验 `model_invocable`。Skill 启用状态不形成额外权限审批；
  `load_skill` 自身直接放行，激活后执行的文件、Shell 等真实工具继续走各自 Authorizer。
- 每个主 Run 和每个 child execution 都持有独立 `SkillActivationLatch`。成功调用先按 ToolCallId 暂存，
  同批重复或历史已激活名称返回 `already_active`，不会提前修改 Session/child 的权威 ledger。
- Recorder 按 Tool Result 原顺序收集已成功暂存的定义，通过 `InternalBoundaryCoordinator` 构造一条隐藏
  Runtime UserMessage；Tool Results、该消息和 Activation ledger 必须由 Store 作为同一完成事实提交，
  Store/Journal 成功后才提交 latch。
- 只有上述完成事实可靠落账后，Recorder 才向 Core 返回通用 ContextChanged continuation。Runtime
  保持原业务 RunId，扣减可靠 consumption，并使用单调递增的下一全局 step 和最新 Journal 建立新的
  AgentExecution；Core 不感知 Skill、Session 或 Run。
- main Session 的活动技能投影只读取 main owner ledger；child activation 只进入对应 child conversation，
  父子及兄弟之间不共享 latch，也不把模型激活伪装成真实用户输入。
