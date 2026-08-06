# agent-sdk 模块约束

## 模块定位

`crates/agent-sdk` 是 Agent Core 的候选便利装配入口。它把冻结的 Agent 执行配置收敛为
一个薄 Facade，使普通宿主不必重复拼装 `ExecutionSpec`，最终仍调用唯一的
`AgentExecution::start`。

普通使用与候选公共 API 分层见
[`Agent SDK 导读`](../Agent-SDK导读.md)。
当前 target 的性能、平台/Provider 证据等级和验收映射见
[`v0.7.0 兼容性与性能基线`](../versions/v0.7.0/兼容性与性能基线.md)。

修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`Agent 系统技术架构`](agent-system.md)
- [`agent-core 模块约束`](agent-core.md)
- 当前版本功能设计、技术方案和开发计划

## 依赖与职责

- 只直接依赖自身公共入口实际使用的抽象 crate：`agent-core`、`agent-context`、
  `agent-model`、`agent-tools` 和必要的 `agent-types`。
- 不依赖具体 Provider、`agent-tools-local`、Tauri、Axum、正式 Runtime、应用协议或任一 Demo。
- `agent-testkit` 只能作为 dev-dependency，用于离线测试和 examples。
- SDK 可以定义 Builder 校验和无持久化的临时 Recorder，但不复制 Agent Loop、事件、终态、
  Tool Dispatcher、Context 算法或 Provider Codec。

## Agent 生命周期

- 一个 `Agent` 是冻结的执行配置与启动 Facade，持有 Model、System Prompt、Context Window、
  ToolSet、请求配置、Budget 和 Guardrail，不持有动态 Conversation。
- `Agent` 不拥有 SessionId、RunId、Journal、审批状态、Memory Store、调度器或配置文件。
- 同一会话的上下文变更执行由上层串行；SDK 不用内部锁伪造未知的 Session 所有权。
- 不同 Agent 可以共享线程安全的 ModelService、Transport 和只读能力，并由 Tokio 异步并发。
- Pinned Memory 在进入 SDK 前已经渲染进冻结 Prompt；Recall 和记忆修改只作为普通工具进入
  ToolSet，SDK 不直接访问 `agent-memory` API。

## 普通入口

- `AgentBuilder::new(model, system_prompt, context_window)` 固定三个必填能力；工具、模型请求
  配置、预算和 Guardrail 通过消费式方法显式覆盖。
- Builder 默认使用空 ToolSet、`ModelRequestConfig::default()`、全 `None` Budget 和关闭的
  Guardrail，不注入隐藏执行上限。
- `build` 只校验上下文窗口、tool-call/reasoning 能力与 ToolChoice/ToolSet 的跨字段关系；
  generation 数值和 Provider Options 内容仍由具体 Adapter 校验。
- `Agent::start` 接收完整 `ExecutionContext`，供 Runtime 风格宿主绑定权威 Recorder；
  `Agent::start_ephemeral` 仍要求显式 Authorizer，但为本次执行创建独享的临时 Recorder。
- 临时 Recorder 不是 Noop：它校验 pending 重入和 receipt 匹配，并接收取消收敛时补齐的完整
  ToolResult 批次；结算后立即丢弃内容，不提供读取、持久化或异常恢复能力。
- `Agent` 只提供冻结 System Prompt 和工具定义的只读访问，不公开可变 `ExecutionSpec`。

## 公共 API 边界

- 顶层只重导普通构建和执行路径直接需要的类型，不提供 wildcard prelude。
- SDK 自有公共面只有 `AgentBuilder`、`Agent` 与 `AgentBuildError`；其余顶层类型均为普通
  启动路径所需的精选重导，不在 SDK 内创建镜像类型。
- Provider Transport/Codec、文件/Shell 请求、Memory 领域类型、Dispatcher、策略 Adapter 和
  testkit 从所属 crate 导入，不经 SDK 镜像或批量重导。
- SDK 自有错误必须结构化、脱敏，并保留底层错误来源；不得携带 prompt、credential、
  Provider Options 正文或完整工具 schema。
- 无持久化入口必须明确命名为临时路径，不得让调用方误认为具备会话恢复能力。
- 候选 API 审查必须基于根导出、反向依赖树和真实调用点；没有证据时不批量收窄可见性、增加
  `non_exhaustive` 或修改 serde 形状。

## 不应放在本模块的内容

- Session/Run 创建、排队、并发门禁、恢复和持久化。
- Conversation Journal、审批交互、策略保存、审计和 Trace Collector。
- 具体 Provider、本地文件/Shell、数据库、网络工具或桌面能力实现。
- Context Checkpoint、压缩 continuation、Pinned Store 或 RecallSource 编排。
- HTTP DTO、SSE、页面状态和产品配置快捷方法。

## 验证

```bash
cargo tree -p agent-sdk --depth 3
cargo check -p agent-sdk
cargo test -p agent-sdk
cargo clippy -p agent-sdk --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
