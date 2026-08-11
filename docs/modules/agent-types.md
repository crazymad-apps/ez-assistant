# agent-types 模块约束

## 模块定位

`crates/agent-types` 承载 Provider-neutral、无 I/O 的 Agent 规范类型。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 只定义消息、内容 Part、Tool 协议值、标识、usage、finish reason 等稳定值类型。
- 不定义服务 trait、Registry、全局状态和运行时句柄。
- 不依赖 Tokio、HTTP、Provider SDK、Tauri、Runtime、数据库或 `assistant-protocol`。
- Provider 原生 schema 不得进入本 crate；不透明 Provider 状态必须有 Provider、Protocol、类型和格式版本边界。
- 公共不变量使用受控构造并覆盖序列化 round-trip 测试。
- `ConversationSnapshot` 是 ToolCallId 唯一性、Tool Call/Result 双向一一配对和结果
  顺序的唯一校验入口；Context、Provider 和 Harness 只能复用，不能各自实现。
- `ContextSummaryMessage` 是明确的派生上下文类型，不伪装成 User/Assistant，也不保存
  策略、模型或 usage。
- `UserPart::FileReferences` 是持久化的用户可见 Part，只保存原始文件名与
  Agent 稳定可读路径；不包含应用层 Attachment ID、文件正文、Base64 或解析结果。

## 验证

```bash
cargo test -p agent-types
cargo clippy -p agent-types --all-targets --all-features -- -D warnings
```
