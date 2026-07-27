# agent-tools 模块约束

## 模块定位

`crates/agent-tools` 承载 Agent 工具 SPI、注册表、派发器、文件/Shell 能力契约与内置工具桥接。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

本 crate 只定义工具能力与规范 Tool Call 的派发契约；真实文件系统与 Shell 副作用由 Runtime/Adapter 实现，并在装配期经桥接工具注册进 Registry。

## 职责

- 类型化 Tool 抽象与类型擦除：输入经 serde 反序列化即校验，JSON Schema 由 schemars 派生。
- `ToolRegistry`（构建期注册，重名拒绝；定义注册时冻结）与 `ToolSetSnapshot`（不可变，随 `ExecutionSpec` 进入执行）。
- 单次规范 Tool Call 派发：按名称查表、反序列化输入、调用工具、产出规范 Tool Result。
- 文件与 Shell 能力契约（trait、输入输出类型、错误语义）。
- 内置工具桥接：把能力实现包装为类型化 Tool 的构造器。

## 核心约束

- 只依赖 `agent-types` 与技术方案 §3.2 列明的第三方 crate；不依赖 `agent-core`、Tokio 运行时句柄、Tauri 或 Runtime。
- 不使用 `async-trait`；异步 trait 方法手写 boxed future（沿 `ModelService` 先例）。
- 类型化工具经 `register` 注册时，`input_schema` 固定由 schemars 从 `Input` 派生（擦除层强制，不依赖实现方自觉）；经 `register_erased` 注册的自定义实现自行负责 schema 与反序列化规则的一致性。
- `ToolDefinition` 在注册时读取一次并与工具句柄共同冻结；重名检查、快照定义与名称索引都基于冻结定义。
- `ToolError` 仅 `InvalidInput { message }`（校验/参数失败）与 `Execution { message }`（执行失败），均转为错误 `ToolResult` 回喂模型；取消不是 `ToolError`，由 `ToolContext.cancellation` 观察处理。
- 能力契约只描述能力形状与错误语义，不含任何安全策略（权限、确认、审计归 Runtime/Adapter 与 Authorizer）。
- 本 crate 不访问 `std::fs`、`tokio::fs`，不启动子进程；能力实现与桥接工具注册由 Runtime 装配。
- Shell 契约：stdin 封闭、无后台执行、命令原样进入审计；敏感环境变量默认不传给子进程；超时、输出上限、取消与进程树清理由实现侧负责。

## 文件与 Shell 能力契约

文件能力统一抽象为读取、列目录、内容搜索、写入、删除与局部编辑（edit）：

```rust
pub trait FileSystemTool: Send + Sync {
    fn read<'a>(&'a self, request: ReadFileRequest) -> FsFuture<'a, ReadFileResult>;
    // list / search / write / delete / edit 同形态（FsFuture<'a, T> 为 boxed-future 别名）
}
```

- `grep`/`rg` 语义归入 `search`，契约不感知搜索后端，`rg` 仅可作为实现侧内部细节。
- `FileToolError` 提供 `NotFound` / `InvalidInput` / `Io { message }` 等稳定分类。
- Shell 是独立一等能力，接受完整命令并发出流式 stdout/stderr；不按每条系统命令枚举专用工具，专用工具只在确实需要结构化领域结果时存在。
- 结构化文件能力与 Shell 并存，不能用 Shell 取代文件能力。

## 不应放在本模块的内容

- Agent Loop、状态机、Recorder/Authorizer SPI、执行事件与预算（归 `agent-core`）。
- 真实文件权限、Shell 确认交互、审计存储与进程管理实现（归 Runtime/Adapter）。
- 规范消息、Tool Call/Result 值类型（归 `agent-types`）。

## Harness 验证

- 单测自带 mini fake 能力实现，不 dev-depend `agent-testkit`（避免依赖环）。
- 覆盖：schema 校验失败转 `InvalidInput`、未知工具名派发、Registry 重名拒绝、快照不可变。

```bash
cargo test -p agent-tools
cargo clippy -p agent-tools --all-targets --all-features -- -D warnings
```
