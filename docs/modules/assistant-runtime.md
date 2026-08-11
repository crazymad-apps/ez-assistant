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
- 按应用配置装配 PinnedMemoryStore、MemoryRecall 与 RecallSource，并通过普通工具注册给 Core。
- 定时任务、配置加载、后台任务与恢复。
- 装配真实文件/Shell Adapter，并负责授权、确认、环境策略与审计。
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

- Runtime 装配本地 `FileSystemTool` Adapter，负责工作目录、能力范围、确认和审计；
  Adapter 负责真实路径解析、符号链接操作、大小限制和 I/O。
- `search` 可以调用打包的 `rg` 或 Rust 搜索后端，必须使用参数数组启动程序，不通过 `sh -c` 拼接查询。
- 写入优先采用同目录临时文件和原子替换；覆盖、删除等策略由权限模式决定。
- 文件授权只约束结构化文件工具，不能宣称约束 Shell。

## Shell 能力

- Shell 是通用工具，不维护不可持续的完整命令白名单。
- Runtime 装配本地 Shell Adapter，并注入工作目录、环境过滤、权限模式与审计策略；
  Adapter 负责进程启动、输出、超时、取消和进程树清理。
- 支持 `Disabled`、逐次确认、会话信任、工作区信任和完全信任等策略；工作区信任不是 OS 沙盒。
- 执行记录包含原始脚本、cwd、开始/结束时间、退出码和确认依据。
- stdout/stderr 流式输出并限制总量；支持超时、取消和进程树清理。
- 默认不向子进程传递模型 API Key、应用令牌等敏感环境变量。
- 风险分析只提供提示，不能作为强安全边界。

## 配置与持久化

- 配置来源、默认值、覆盖顺序和校验必须显式。
- 配置合并结果只生成一份 `ExecutionSpec`；Core 不再二次解释 Profile、默认值或覆盖关系。
- 会话、消息、Run、定时任务和审计记录由 Repository trait 隔离具体存储。
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
  Ask/Auto、规则保存、审批交互和审计仍属于未来 Runtime 的上层编排职责。
- 真实文件与 Shell 机制由独立本地基础设施 Adapter 提供，未来 Runtime 负责注入
  工作目录、能力策略、环境过滤、审批和持久审计；Adapter 不反向定义 Runtime API。
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
  同时编译 `RunToolBundle` 中的 ToolSet 和 Authorizer；二者在该 Run 内保持不可变，
  不进入 Protocol、Conversation 或持久化正文。不同 Workspace 不得共用一个可变全局
  `SessionPathResolver`。
- Runtime 只定义按 Run 装配端口和脱敏错误映射，不直接依赖 `agent-tools-local`。
  绑定 Workspace 的工作目录在 Run 前不可用时返回 `WorkspaceUnavailable`；其他工具构造
  失败按 Agent 构造失败处理，不向客户端泄露路径或底层 Adapter 错误。
- Workspace 假删只阻止新 Session 绑定；已绑定 Session 继续使用其冻结路径。
  附件正文不自动进入模型上下文，Agent 通过持久化 File References 和文件工具按需读取。

## Harness 验证

- 覆盖不同 Session 并发、同 Session 串行、取消、排队、订阅断开、重启恢复和 Scheduler 补跑策略。
- 文件和 Shell 测试使用临时目录与无破坏命令，不访问用户真实文件。

```bash
cargo test -p assistant-runtime
cargo clippy -p assistant-runtime --all-targets --all-features -- -D warnings
```
