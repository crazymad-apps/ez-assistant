# agent-openai-compatible 模块约束

## 模块定位

`crates/agent-openai-compatible` 承载 OpenAI-compatible 协议族的模型 Adapter。当前分别实现
OpenAI Chat Completions compatible 与 Responses API HTTP/SSE；两个协议拥有独立的原生 Schema、
Codec、流状态机和 Service，不能通过单次请求失败后互相回退。修改前必须阅读
[`Agent 系统技术架构`](agent-system.md)、
[`agent-model`](agent-model.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 目录与依赖边界

- `src/chat/` 只承载 Chat Completions 的 `ChatProtocolAdapter`、原生 Schema、Codec、
  流式组装和 `OpenAiChatCompletionsService`。
- `src/responses/` 只承载 Responses 的 `ResponsesProtocolAdapter`、原生 Schema、Codec、
  item 流式组装和 `OpenAiResponsesService`；不得复用 Chat message/choice/delta 枚举模拟 Responses。
- `src/shared/` 只承载已有稳定复用价值的 credential、endpoint、HTTP Transport、SSE parser
  和工具 Schema 降级器，以及已形成双协议调用方的图片 Data URL helper；依赖方向固定为
  `chat/responses -> shared`，shared 不依赖任一协议 Schema 或状态机。
- Chat/Responses fixture 分别位于 `fixtures/chat/`、`fixtures/responses/`，测试位于协议或共享
  基础设施各自的 `tests/` 目录。

## 核心约束

- OpenAI-compatible 原生 schema、协议 Adapter、Codec、Transport 和 Decoder 都封装在本 crate；
  服务 Route 与 capability 事实由 Runtime 编译后注入。
- Adapter 只依赖 `agent-model`/`agent-types`，禁止依赖 `agent-core`、Runtime 和 Tauri。
- credential 通过构造依赖注入，不进入请求 DTO、事件、Debug 和错误文本。
- base URL 不接受 userinfo、query 或 fragment；当前认证只通过 credential 注入
  Authorization header，安全 wire 快照在构造时永久排除认证 header。
- `OpenAiChatCompletionsService::new` 与 `with_transport` 都是可失败构造，统一返回
  `OpenAiChatCompletionsServiceError`；无效 URL 错误不得回显可能包含 credential 的原始 URL。
- Chat Provider 差异通过显式 `ChatProtocolAdapter` 表达，不在 Core 中按名称分支。
- `ChatProtocolAdapter::vllm()` 承担 vLLM Chat Completions 方言：响应优先读取当前
  `reasoning`，缺失或为 null 时兼容旧版 `reasoning_content`；历史回放只写当前字段
  `reasoning`。响应读取字段与请求回放字段必须分离，不能为了兼容别名把同一 reasoning
  重复追加。
- DeepSeek thinking 工具调用必须维持可回放的 `reasoning_content`、`tool_calls` 和
  `tool_call_id`。
- `ChatProtocolAdapter::deepseek()` 明确表示 thinking-enabled 形态；经 `provider_options` 关闭
  thinking 不属于该 Chat 方言的支持范围，不能一边关闭 thinking 一边沿用其 reasoning
  必填校验。
- DeepSeek 偶发返回不带 `reasoning_content` 的合法 Tool Call 时，解码器保留
  该 Tool Call，不伪造规范 `ReasoningPart`；后续回放由 DeepSeek Chat 方言编码器
  补入仅用于 wire 的 reasoning 占位字段。该兼容逻辑不得进入 Core、UI 或
  Journal，也不得应用到不接受 `reasoning_content` 的其他 Chat 方言。
- Context Summary 编码为带固定派生说明的 system message。
- User Message 的 `Text`、旧 `Injected`、新 `InternalContext` 和 `FileReferences` 按规范 Part 顺序编码为
  原生 text content parts；File References 使用确定 XML 文本格式并转义 name/path，
  不伪造 Tool Call、Tool Result 或已读取文件的事实。
- `QuotedText` 在 Chat Completions 与 Responses 中按相同稳定 XML 文本投影，保留一次冻结的纯文本
  exact 和每侧最多 128 个 Unicode 字符的 prefix/suffix；所有文本字段必须转义。quote ID、owner、generation、来源
  Message ID、UTF-16 range、availability 和展示标签都不得进入 Provider wire，也不提示模型调用引用
  专属 Recall 工具。
- Chat Completions 与 Responses 都不得把 `UserMessageOrigin`、`TranscriptVisibility` 编码到
  Provider wire；隐藏 Runtime User Message 仍按普通 user role 及原有 Part 顺序发送。
- `ModelRequest.system` 继续按 `SystemPromptSnapshot::parts()` 保存冻结边界；Chat Completions
  wire 必须把全部 Snapshot Part 和紧随其后的前导 Conversation System/Context Summary 按原顺序以
  `\n\n` 合并成唯一一条开头 system message，兼容只允许 `messages[0]` 为 system 的严格模板。
  Responses 继续把 Snapshot Part 合并进单个 `instructions`。两种投影都不得反向改写 Snapshot、
  Conversation 或持久化事实，也不得把对话中间的 system 事实静默前移。
- 请求编码前复用 `ConversationSnapshot` 的严格 Tool Call/Result 双向校验，不在
  Adapter 内维护第二套配对算法。
- Chat 编码器读取统一的有序 Tool Result Parts：纯 Text/JSON 结果继续确定性编码为字符串。
  `AggregatedUserInput` 路径必须按 Assistant Tool Call 顺序先输出完整整批 tool messages，Image
  Part 在字符串中留下带 `call_id + part_index` 的版本化占位；随后至多追加一条 wire-only user
  image envelope，并按 Tool Call、Part 顺序排列“标签文本 → 图片”。不得逐结果穿插 user message。
- Chat 与 Responses 的聚合图片信封必须先由 `agent-model` 从完整规范 Tool Result 批次生成同一份
  request-only insertion plan；协议编码器只负责把该计划映射为自己的 wire content，不自行决定插入位置。
- wire-only 图片 envelope 只由当前规范 Tool Result 确定性重建，不分配 MessageId，也不能回写
  Conversation、Journal、Recall、事件或产品消息计数。投影为 `Unsupported`、图片未准备或资源
  不匹配时必须在建流前失败，不能静默丢图或动态改走另一投影。
- Service 构造时显式接收模型上下文窗口；请求设置输出上限时，直接根据 `ChatProtocolAdapter` 的
  `max_output_tokens_field` 编码或返回配置错误。
- Context Overflow 只根据可审阅 fixture 中确认的结构化 `error.code` /
  `error.type` 精确映射；当前 allowlist 为 `context_length_exceeded`，禁止通过
  message 文本模糊匹配。HTTP 非 2xx 在建立前返回错误，SSE 结构化错误帧以
  `TurnFailed` 传播。
- Transport 必须保留 Connect、Timeout、Interrupted 分类；429、408、425 与 5xx 提供
  provider-neutral 限流/暂时不可用事实和可解析的 Retry-After，不把分类先折成字符串。
- 流式模型请求的 `request_timeout` 约束等待响应建立和相邻响应正文 chunk 的空闲时间，
  每收到一个 chunk 都重新计时；不得把它配置为覆盖完整 SSE 生命周期的总时限。响应建立前
  Timeout 可进入上层有限重试，响应建立后的空闲 Timeout 只能结束当前 Step，不透明重试。
- Provider wire 观察位于 Transport 装饰器：记录编码后安全请求、允许的响应头、原始 chunks、
  EOF 和 TransportError，同时原样转发请求、分块、取消与错误。观察器不得写文件或改变结果。
- wire body/chunk 在内存中保持原始 bytes，serde 使用 Base64 紧凑表示；相同规范请求与相同
  `ChatProtocolAdapter` 的编码必须确定性产生相同 request bytes。
- 默认测试完全离线；真实 API 只能作为显式忽略的 smoke test。
- 对外只暴露字段私有的 `ChatProtocolAdapter` 具名构造，不允许 Runtime 用字段字面量拼接方言；
  reasoning 历史使用 `Drop`、`ToolCallsOnly`、`PreserveAll` 策略表达，不再使用模糊布尔开关。
- `OpenAiChatCompletionsService::new_with_capabilities` 接收 Runtime 唯一编译的能力事实；协议 Adapter
  仍独立校验 wire 约束，不能根据一次响应反推或修改模型能力。
- `OpenAiResponsesService` 首版固定请求 `/responses`、`store: false`、`stream: true`，每次从本地
  规范 Conversation 编码完整历史；不发送 `previous_response_id`、远端 `conversation`、后台模式
  或其他服务端会话状态。
- 规范 `ReasoningPart.id` 只用于本地片段聚合，不等同于 Provider 原生 Responses item ID。
  精确原生 item 只能从相容的 `ProviderState` 恢复；DeepSeek 的无状态 Responses 历史使用无
  `id`/`summary` 的明文 `reasoning_text` content，不能把本地 ID 冒充为 Provider item ID。
  其他方言只沿用各自已经验证的普通 reasoning 投影。
- Responses 编码单个规范 Assistant Turn 时，不能把 Chat 流式 part 的到达顺序直接解释为原生
  item 边界；必须稳定投影为 reasoning、assistant text、完整 function-call 批次，再由外层追加
  对应 function-call outputs。中间 reasoning/message 不得切断同一批工具调用。
- Responses 的 `instructions`、用户/助手文本、图片输入、Context Summary、function call/output、
  reasoning、refusal、usage 和终态均由独立 item 状态机映射。并行 function call 按
  `output_index` 稳定完成；未知 item、缺失终态、坏参数、终态后额外数据和身份冲突均 fail-closed。
- Responses 原生 `function_call_output` 是否可携带 content parts 由显式 `FunctionOutputShape`
  方言事实决定；String-only 路由使用已验证的批次聚合图片输入，不在运行期试错或切换投影。
- Responses 精确具名方言包括 DeepSeek Flash/Pro/Vision、DashScope Qwen `qwen3.8-max` 和 Moonshot
  Kimi `k3`；未知兼容端点仍使用保守通用方言。Qwen 工具图片固定使用批次聚合 User Image，Kimi
  与 DeepSeek `deepseek-v4-flash-vision-exp` 固定使用原生 content-parts function output；DeepSeek
  非视觉 Flash/Pro 不启用图片，不得运行期试错切换。
- OpenAI、DeepSeek 与 Kimi 的非空 encrypted reasoning 保存为完整原生 item，并同时生成规范
  `ReasoningPart`。回放只接受 provider、protocol、规范 endpoint、model、格式和 related part
  完全相容的状态；相容 payload 损坏或与规范 reasoning 矛盾时 fail-closed，路由不相容时跳过
  opaque item，并只按目标方言明确声明的普通 reasoning 形状投影规范 part。Qwen/Kimi 的
  `encrypted_content: null` 不生成空状态。
- Responses 流状态机同时接受专用 reasoning 事件和 DeepSeek 使用的通用
  `response.content_part.added/done` reasoning 边界。OpenAI-compatible 工具 Schema 降级对无字段
  object 显式输出 `properties: {}`，满足 DashScope 的 Responses 校验而不改变规范 ToolDefinition。
- DashScope Qwen 使用 `response.reasoning_text.*` 流式发送最终 `summary`，Decoder 必须从首个
  增量开始将两者归入同一规范 `ReasoningPart`，不能把流式正文和完成快照重复落账。

## 验证

```bash
cargo test -p agent-openai-compatible
cargo clippy -p agent-openai-compatible --all-targets --all-features -- -D warnings
```
