# agent-provider-openai-compatible 模块约束

## 模块定位

`crates/agent-provider-openai-compatible` 将规范模型契约适配到 OpenAI Chat Completions compatible HTTP/SSE 协议。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-model`](agent-model.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- Provider 原生 schema、Route/Profile、Codec、Transport 和 Decoder 都封装在本 crate。
- Adapter 只依赖 `agent-model`/`agent-types`，禁止依赖 `agent-core`、Runtime 和 Tauri。
- credential 通过构造依赖注入，不进入请求 DTO、事件、Debug 和错误文本。
- base URL 不接受 userinfo、query 或 fragment；当前认证只通过 credential 注入
  Authorization header，安全 wire 快照在构造时永久排除认证 header。
- `OpenAiCompatibleService::new` 与 `with_transport` 都是可失败构造，统一返回
  `OpenAiCompatibleServiceError`；无效 URL 错误不得回显可能包含 credential 的原始 URL。
- Provider 差异通过显式 Profile 表达，不在 Core 中按名称分支。
- DeepSeek thinking 工具调用必须完整往返 `reasoning_content`、`tool_calls` 和 `tool_call_id`。
- `Profile::deepseek()` 明确表示 thinking-enabled 形态；经 `provider_options` 关闭
  thinking 不属于该 Profile 的支持范围，不能一边关闭 thinking 一边沿用其 reasoning
  必填校验。
- 当 Profile 要求工具调用携带 reasoning 时，流式和非流式响应都必须在产出完整
  `AssistantMessage` / `TurnFinished` 前校验该不变量；缺失时以 Provider 协议错误终止
  当前 Turn，不能把无法回放的 Tool Call 交给 Core 执行或写入 Journal。
- Context Summary 编码为带固定派生说明的 system message。
- `ModelRequest.system` 按 `SystemPromptSnapshot::parts()` 的冻结顺序逐条编码；透明快照
  不改变既有线上 JSON 和 system message 顺序。
- 请求编码前复用 `ConversationSnapshot` 的严格 Tool Call/Result 双向校验，不在
  Adapter 内维护第二套配对算法。
- Service 构造时显式接收模型上下文窗口；请求设置输出上限时，直接根据 Profile 的
  `max_output_tokens_field` 编码或返回配置错误。
- Context Overflow 只根据可审阅 fixture 中确认的结构化 `error.code` /
  `error.type` 精确映射；当前 allowlist 为 `context_length_exceeded`，禁止通过
  message 文本模糊匹配。HTTP 非 2xx 在建立前返回错误，SSE 结构化错误帧以
  `TurnFailed` 传播。
- Transport 必须保留 Connect、Timeout、Interrupted 分类；429、408、425 与 5xx 提供
  provider-neutral 限流/暂时不可用事实和可解析的 Retry-After，不把分类先折成字符串。
- Provider wire 观察位于 Transport 装饰器：记录编码后安全请求、允许的响应头、原始 chunks、
  EOF 和 TransportError，同时原样转发请求、分块、取消与错误。观察器不得写文件或改变结果。
- wire body/chunk 在内存中保持原始 bytes，serde 使用 Base64 紧凑表示；相同规范请求与相同
  Profile 的编码必须确定性产生相同 request bytes。
- 默认测试完全离线；真实 API 只能作为显式忽略的 smoke test。

## 验证

```bash
cargo test -p agent-provider-openai-compatible
cargo clippy -p agent-provider-openai-compatible --all-targets --all-features -- -D warnings
```
