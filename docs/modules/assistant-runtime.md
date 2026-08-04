# assistant-runtime 模块约束

## 模块定位

`crates/assistant-runtime` 是应用业务运行时和权威状态所有者。第一阶段直接运行在 Tauri Rust 进程内，不是 sidecar 或本地服务。

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
Tauri 进程
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

## Harness 验证

- 覆盖不同 Session 并发、同 Session 串行、取消、排队、订阅断开、重启恢复和 Scheduler 补跑策略。
- 文件和 Shell 测试使用临时目录与无破坏命令，不访问用户真实文件。

```bash
cargo test -p assistant-runtime
cargo clippy -p assistant-runtime --all-targets --all-features -- -D warnings
```
