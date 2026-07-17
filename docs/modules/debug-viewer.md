# debug-viewer 模块约束

## 模块定位

`tools/debug-viewer` 是网页端实时查看 LLM 数据流的独立开发工具：viewer server（Rust bin，`POST /ingest` 接收调试信封、SSE `/events` 广播、`ServeDir` 伺服静态页）+ 出向推送客户端（`DebugClient`）。它是独立进程，不属于产品架构；产品进程只向它建立出向连接，不监听调试端口（决策见 [`重要决策与变更记录`](../重要决策与变更记录.md)）。修改前必须阅读 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 只读依赖 `agent-model` / `agent-types` 的规范类型；不得被产品 crate 以非 dev-dependency 方式依赖（当前唯一使用方是 provider crate 的 `examples/chat.rs`）。
- 线格式（`DebugEnvelope` / `DebugPayload` / `BroadcastMessage`）归本 crate 所有，不进入 `assistant-protocol`；`DebugChannel::Sched` 预留给 Runtime 调度事件（v0.2.0）。
- 推送端 fire-and-forget：`post` 只做有界队列的非阻塞入队，HTTP 发送由独立后台任务串行执行（连接/请求短超时）；队列满、发送失败或通道关闭时一次性告警后自静音，绝不影响主流程；server 只绑定 loopback。
- credential 与请求/响应 header 永不进入调试 payload；消息正文、tool arguments 属于调试内容，由本地 loopback 承载。
- 静态页由 `ServeDir` 伺服 `public/` 目录（根路径在编译期固定为本 crate 目录，运行时读盘，页面改动刷新浏览器即生效）；默认端口 7331，`DEBUG_PORT` 可覆盖。

## 验证

```bash
cargo test -p debug-viewer
cargo clippy -p debug-viewer --all-targets --all-features -- -D warnings

# 手动端到端：
cargo run -p debug-viewer
cargo run -p agent-provider-openai-compatible --example chat -- --debug http://localhost:7331
# 浏览器打开 http://localhost:7331
```
