# debug-viewer 模块约束

## 模块定位

`tools/debug-viewer` 是网页端实时查看模型、Agent 与 Runtime 数据流的独立开发工具：viewer server（Rust bin，`POST /ingest` 接收调试信封、SSE `/events` 广播、`ServeDir` 伺服静态页）+ 出向推送客户端（`DebugClient`）。它是独立进程，不属于产品架构；产品进程只向它建立出向连接，不监听调试端口（决策见 [`重要决策与变更记录`](../重要决策与变更记录.md)）。修改前必须阅读 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 正式代码只读依赖 `agent-core` / `agent-model` 的规范类型，测试可直接依赖
  `agent-types`；不得被产品 crate 以非 dev-dependency 方式依赖。provider crate 的
  `examples/chat.rs` 通过 dev-dependency 使用，独立开发工具 `runtime-harness`
  可普通依赖。
- 线格式（`DebugEnvelope` / `DebugPayload` / `BroadcastMessage`）归本 crate 所有，不进入 `assistant-protocol`；一个 viewer 始终接收 `llm` / `agent` / `runtime` 三通道，不通过启动项固定通道。
- `DebugPayload::AgentEvent` 直接承载 `agent_core::AgentEvent`；`RuntimeEvent` 只承载事件名和结构化调试数据。二者不得改变网页中模型 Turn 卡片的生命周期。
- 用户输入只由 Runtime 的 `user_message_appended` 产生独立消息卡；LLM
  `TurnRequested` 仅提供可展开的请求快照，不从其中重复推导用户消息。
- 推送端 fire-and-forget：`post` 默认发送 `llm`，`post_on` 可指定通道；两者只做同一有界队列的非阻塞入队，共享序号、关联 ID 和静音状态。HTTP 发送由独立后台任务串行执行（连接/请求短超时）；队列满、发送失败或通道关闭时一次性告警后自静音，绝不影响主流程；server 只绑定 loopback。
- credential 与请求/响应 header 永不进入调试 payload；消息正文、tool arguments 属于调试内容，由本地 loopback 承载。
- 静态页由 `ServeDir` 伺服 `public/` 目录（根路径在编译期固定为本 crate 目录，运行时读盘，并统一返回 `Cache-Control: no-store`，确保普通刷新读取当前源码）；顶部 Session Tab 将 `<session>/<run>` correlation 按 session 部分聚合，同一会话的多次 Run 只显示一个 Tab，模型流卡片仍按完整 correlation 隔离；左侧固定展示模型输出，右侧以 `llm` / `agent` / `runtime` 单选 Tab 切换事件：LLM 展示原始事件，Agent/Runtime 展示轻量时间线并可展开完整原始 JSON。默认端口 7331，`DEBUG_PORT` 可覆盖。

## 验证

```bash
cargo test -p debug-viewer
cargo clippy -p debug-viewer --all-targets --all-features -- -D warnings

# 手动端到端：
cargo run -p debug-viewer
cargo run -p agent-provider-openai-compatible --example chat -- --debug http://localhost:7331
# 浏览器打开 http://localhost:7331
```
