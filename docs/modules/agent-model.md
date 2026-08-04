# agent-model 模块约束

## 模块定位

`crates/agent-model` 定义 Provider-neutral 的单次模型 Turn 契约。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-types`](agent-types.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- `ModelService` 一次只调用一个 Provider Turn，不执行工具或自动继续 Agent Loop。
- 请求只携带语义输入；调用目标（endpoint、credential、model）归服务实例构造期参数，一个服务实例即一条编译完成的模型配置，请求与上下文不携带路由信息。
- `SystemPromptSnapshot` 保存有序、已完成渲染的最终 system parts，使用透明数组序列化；
  只提供整体构造、只读访问和消费，不提供原地修改接口。
- `ModelRequest.system` 必须使用冻结快照；调用方不能在单次 AgentExecution 的不同
  Model Step 中重新读取 Store 或动态改写该前缀。
- 规范事件具有明确 Part 生命周期和唯一终态。
- 请求和调用上下文不携带业务 `RunId`、应用 Session 或 credential 明文。
- 错误区分建立前失败、流中失败和取消，不泄露 prompt、credential 或完整响应正文。
- 建立错误提供 Connection、Timeout、RateLimited、Unavailable 与 Retry-After 等稳定事实，
  不通过展示文本解析重试资格；流中失败继续只通过唯一 `TurnFailed` 终态表达。
- `TraceContext` 只承载逻辑模型调用与 attempt 的诊断关联，不代表业务 Run，也不进入
  Provider HTTP header。
- `RetryingModelService` 只包装 `stream()` 直接错误，策略必须显式且 attempt 有限；一旦
  获得事件流就透明返回，绝不重试后续失败。
- 重试等待必须响应取消，复用完全相同的 `ModelRequest`，并透明委托 capabilities 与
  context window；Core 不感知 attempt。
- `ModelService` 直接暴露当前实例绑定模型的 `context_window_tokens`；调用方不得按
  provider/model 名称猜测窗口。
- Provider 明确报告上下文过长时使用 provider-neutral `ContextOverflow`；建立前与
  流内仍遵守既有错误边界。
- `GenerationConfig.max_output_tokens` 只表示本次请求的输出上限；是否能编码由具体
  Adapter 的方言配置判断，不在 `ModelCapabilities` 重复声明。
- 不依赖 Tauri、Runtime、数据库和具体 Provider Adapter。
- 不持久化 Trace；attempt observer 只报告原生事实，记录失败不能改变模型结果。

## 验证

```bash
cargo test -p agent-model
cargo clippy -p agent-model --all-targets --all-features -- -D warnings
```
