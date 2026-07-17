# agent-model 模块约束

## 模块定位

`crates/agent-model` 定义 Provider-neutral 的单次模型 Turn 契约。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-types`](agent-types.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- `ModelService` 一次只调用一个 Provider Turn，不执行工具或自动继续 Agent Loop。
- 请求只携带语义输入；调用目标（endpoint、credential、model）归服务实例构造期参数，一个服务实例即一条编译完成的模型配置，请求与上下文不携带路由信息。
- 规范事件具有明确 Part 生命周期和唯一终态。
- 请求和调用上下文不携带业务 `RunId`、应用 Session 或 credential 明文。
- 错误区分建立前失败、流中失败和取消，不泄露 prompt、credential 或完整响应正文。
- 不依赖 Tauri、Runtime、数据库和具体 Provider Adapter。

## 验证

```bash
cargo test -p agent-model
cargo clippy -p agent-model --all-targets --all-features -- -D warnings
```
