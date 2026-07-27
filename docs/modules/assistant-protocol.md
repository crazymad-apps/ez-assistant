# assistant-protocol 模块约束

## 模块定位

`crates/assistant-protocol` 定义 UI 与 Assistant Runtime 之间，以及确有跨层消费者的稳定应用契约。Agent Core 内部的模型事件、规范对话、执行状态和能力 trait 不因复用方便而进入本 crate。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 允许内容

- `SessionId`、`MessageId`、`RunId` 等应用级 newtype ID；`RunId` 由 Runtime 持有，不进入 Agent Core 执行输入。
- Command/request、事件、快照和分页查询 DTO。
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

## Harness 验证

- 序列化 round-trip。
- 关键事件 JSON 快照或明确兼容性测试。
- ID、状态转换和可选字段语义测试。

```bash
cargo test -p assistant-protocol
cargo clippy -p assistant-protocol --all-targets --all-features -- -D warnings
```
