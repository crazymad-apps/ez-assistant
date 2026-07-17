# agent-types 模块约束

## 模块定位

`crates/agent-types` 承载 Provider-neutral、无 I/O 的 Agent 规范类型。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 只定义消息、内容 Part、Tool 协议值、标识、usage、finish reason 等稳定值类型。
- 不定义服务 trait、Registry、全局状态和运行时句柄。
- 不依赖 Tokio、HTTP、Provider SDK、Tauri、Runtime、数据库或 `assistant-protocol`。
- Provider 原生 schema 不得进入本 crate；不透明 Provider 状态必须有 Provider、Protocol、类型和格式版本边界。
- 公共不变量使用受控构造并覆盖序列化 round-trip 测试。

## 验证

```bash
cargo test -p agent-types
cargo clippy -p agent-types --all-targets --all-features -- -D warnings
```
