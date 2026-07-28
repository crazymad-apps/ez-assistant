# agent-core 模块约束

## 模块定位

`crates/agent-core` 是与 UI 和应用生命周期无关的 Agent 执行引擎，负责一次 `AgentExecution` 内的模型调用、工具调用循环、上下文构建和执行事件。业务 Run 及其 `RunId` 属于 `assistant-runtime`。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- Agent Loop 与终止条件。
- 模型 Provider trait 和流式响应抽象。
- 单次 `AgentExecution` 的上下文组装、工具结果回填和事件输出。
- 与执行逻辑直接相关的 token、轮次和工具调用限制。
- 规范对话、Provider Codec、Memory/Safety/Recorder/Authorizer 等稳定能力接口。

Tool 抽象、注册表、派发器与文件/Shell 能力契约归 [`agent-tools`](agent-tools.md)；Core 只消费 `ToolSetSnapshot` 与派发结果。

## 核心约束

- Core 不知道 Tauri、窗口、会话列表、定时任务、SQLite 或配置文件位置。
- Core 不直接调用 `std::fs`、`tokio::fs`、`Command`、HTTP 工具或桌面 API；所有副作用通过注入的模型/工具 trait。
- Core 处理一个 `AgentExecution`，不持有 `RunId`，不负责跨会话排队和全局并发。
- `ExecutionSpec` 是已经由 Runtime 解析完成的不可变执行事实源；Core 不再维护 `AgentProfile`、配置默认值或覆盖顺序。
- 执行必须可取消，模型流和工具调用都要观察取消信号；工具取消后 Core 等待 dispatch 完成资源清理，不直接丢弃 future；取消收敛前为批次内未结算调用补记 interrupted 错误 ToolResult，并原子完成 pending exchange。
- 资源预算使用显式 `Option`；Core 不注入隐藏的最大轮次、超时或输出限制；预算是副作用前硬边界（`max_steps` 模型调用前预检、`max_tool_calls` dispatch 前预检）。
- 启发式 Guardrail 必须支持 `Off`、`Observe`、`Enforce`，不能以未声明规则静默中止执行。
- 工具输入先完成 schema/类型校验，再进入实现。
- Core 接收规范对话快照，不使用 `ConversationRef` 自行加载或持久化 Session。
- 规范对话记录与 UI/诊断事件分离；Provider 特有字段由 Codec 往返保真。
- Recorder 以 pending/completed 两阶段 tool exchange 表达副作用前写入与结果批次原子完成；规范快照不得暴露 pending exchange。
- 普通观察事件允许背压丢弃，唯一终态通过独立通道可靠交付且三种终态都报告丢弃计数。

## 最小可执行 Agent

- 空 `ToolSetSnapshot` 是合法输入：不含任何工具的 Agent 必须可以正常执行并纯文本收尾。
- Core 不内嵌任何工具实现，对工具来源（内置桥接、Runtime 外部注册）无差别；工具装配完全由 Runtime 完成。

## 权限预留（Authorizer）

- 工具副作用顺序固定为 begin pending exchange → authorize → execute → complete batch；Authorizing 是状态机固定位置，Core 只保证逐 Tool Call 独立过闸（Allow 即执行，再处理下一调用），不设"整批放行才执行"的规则。
- 审批编排（串行询问、攒批询问、规则自动放行）归 Runtime authorizer 实现；Core 在 authorize 时提供本轮批次上下文（同轮全部 tool call），批次审批交互由 Runtime 借此自行完成。
- Core 授权决策仅 `Allow` / `Deny { reason }`；`Deny` 在授权闸处转换为错误 `ToolResult`——回喂模型、驱动循环继续的唯一载体是 error `ToolResult`，对模型与循环不存在"被拒绝"类别；reason 措辞归 Runtime。
- `ExecutionContext.authorizer` 为必传字段，类型层面杜绝"无授权闸"的隐藏默认；Core 只提供显式装配用的 `AllowAllAuthorizer`。
- `Ask` 不进 Core 词汇表：审批由 Runtime 的 authorizer 实现内部挂起、经 Runtime 侧审批交互代理完成，审批事件由 Runtime 自行产生。
- authorize 与取消存在 race，必须可断言；recorder 失败阻断后续副作用。

## 文件与 Shell 工具

文件/Shell 能力契约与内置桥接见 [`agent-tools`](agent-tools.md)。Core 通过注入的 Authorizer 完成决策编排；规则保存、用户交互、审计和真实执行约束由 Runtime/Adapter 完成。

## Memory 与 Provider 边界

- Core 只依赖稳定 `MemoryService`，不依赖本地数据库、远程协议或具体记忆算法。
- `MemoryPlugin` 由外部 Orchestrator 组合，支持本地、远程、混合、多来源和 No-op 实现。
- 记忆插件提供模型工具时走普通 Tool Contribution，禁止 Memory SPI 直接依赖 Tool SPI。
- Model Provider 一次只完成一个模型 Turn；工具继续循环属于 Agent Engine。
- reasoning、tool call、tool result 和 Provider continuation state 必须进入规范对话；不能只依赖流式事件恢复。

## 不应放在本模块的内容

- Session 列表、标题、归档和持久化。
- 全局 Run 队列、定时任务、模型账户配置。
- `RunId` 的生成、状态、查询、取消、恢复和事件关联。
- 真实文件权限、Shell 确认界面和审计存储。
- Tauri event/channel 与前端 DTO 转换。

## Harness 验证

- 引擎 Harness 宿主在 `agent-testkit/tests/`（`agent-core` 被 testkit 依赖，不反向 dev-depend）。
- Agent Loop 使用 fake model 和 fake tool 覆盖：纯文本结束、工具调用、多轮工具、模型失败、工具失败和取消。
- 覆盖有限/无限预算与 Guardrail 三种模式，不假定固定最大轮次。
- 覆盖 Memory Plugin 聚合、Recorder begin/complete 失败与 pending 恢复、Authorizer Allow/Deny、授权等待中的取消 race、工具取消清理完成、Provider reasoning/tool-call 往返保真。
- 事件顺序必须可断言，不依赖真实模型网络。
- 可运行效果演示位于 `crates/agent-testkit/examples/engine_demo.rs`。

```bash
cargo test -p agent-testkit
cargo clippy -p agent-core --all-targets --all-features -- -D warnings
```
