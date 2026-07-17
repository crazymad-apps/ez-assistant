# agent-provider-openai-compatible 模块约束

## 模块定位

`crates/agent-provider-openai-compatible` 将规范模型契约适配到 OpenAI Chat Completions compatible HTTP/SSE 协议。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-model`](agent-model.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- Provider 原生 schema、Route/Profile、Codec、Transport 和 Decoder 都封装在本 crate。
- Adapter 只依赖 `agent-model`/`agent-types`，禁止依赖 `agent-core`、Runtime 和 Tauri。
- credential 通过构造依赖注入，不进入请求 DTO、事件、Debug 和错误文本。
- Provider 差异通过显式 Profile 表达，不在 Core 中按名称分支。
- DeepSeek thinking 工具调用必须完整往返 `reasoning_content`、`tool_calls` 和 `tool_call_id`。
- 默认测试完全离线；真实 API 只能作为显式忽略的 smoke test。

## 验证

```bash
cargo test -p agent-provider-openai-compatible
cargo clippy -p agent-provider-openai-compatible --all-targets --all-features -- -D warnings
```
