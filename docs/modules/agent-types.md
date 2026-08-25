# agent-types 模块约束

## 模块定位

`crates/agent-types` 承载 Provider-neutral、无 I/O 的 Agent 规范类型。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 只定义消息、内容 Part、Tool 协议值、标识、usage、finish reason 等稳定值类型。
- 不定义服务 trait、Registry、全局状态和运行时句柄。
- 不依赖 Tokio、HTTP、Provider SDK、Tauri、Runtime、数据库或 `assistant-protocol`。
- Provider 原生 schema 不得进入本 crate；不透明 Provider 状态必须有 Provider、Protocol、类型和格式版本边界。
- `OpaqueProviderState` 还必须绑定规范 `related_part_id` 和由 Adapter 计算的精确路由指纹；旧数据
  可读取为无路由绑定状态，但不得在新请求中作为相容状态回放。payload 单 item 上限为 2 MiB，
  单 Assistant Turn 的 8 MiB 上限由消费方执行；`Debug` 只能展示类型、边界和字节数，不能展示
  payload 或路由指纹。
- 公共不变量使用受控构造并覆盖序列化 round-trip 测试。
- `ConversationSnapshot` 是 ToolCallId 唯一性、Tool Call/Result 双向一一配对和结果
  顺序的唯一校验入口；Context、Provider 和 Harness 只能复用，不能各自实现。
- `ContextSummaryMessage` 是明确的派生上下文类型，不伪装成 User/Assistant，也不保存策略；
  生成摘要的模型身份、摘要请求自身的 usage 和被压缩历史的累计 usage 分字段保存。
- `UserPart::Injected` 只保留旧 Conversation 的读取兼容；新产生的变体指令、Goal、WorkPlan、
  委派和 continuation 使用带 `boundary_id`、`kind` 与可选 `retention_key` 的
  `UserPart::InternalContext`。两者都不是用户正文，但规范 Part 必须随 User Message 持久化，
  不能在会话重建时重新猜测生成。
- `ContextInsertionPlan` 统一表达规范内部 Part、请求末尾临时指令和 Tool Result 批次后
  request-only 图片信封的位置、存储和可见性；计划本身不携带 Runtime RunId 或产品消息身份。
- `UserMessageOrigin` 与 `TranscriptVisibility` 是正交应用元数据：旧 JSON 缺字段时必须分别按
  `User`、`Visible` 读取；`Hidden` 只排除产品转录，不得从规范 Conversation 或模型上下文移除。
  Provider Adapter 不编码这两个字段。
- `UserPart::FileReferences` 是持久化的用户可见 Part，只保存原始文件名与
  Agent 稳定可读路径；不包含应用层 Attachment ID、文件正文、Base64 或解析结果。
- `ToolResultContent` 是非空、有序的 `Text`/`Json`/`Image` Part 集合；新写入统一使用
  `parts` 形状，读取仍兼容旧的单一 `Text`/`Json` 形状并立即归一化。不得在其他 crate
  复制一套旧数据兼容逻辑。
- `ToolImageReference` 只保存 Session 内单层 `<sha256>.<ext>` 相对引用和实际 MIME，字段
  通过受控构造校验；它不保存 Session 根、外部源路径、Base64 或图片字节。首版只允许
  JPEG、PNG、WebP 和静态 GIF 的规范 MIME/扩展名组合。

## 验证

```bash
cargo test -p agent-types
cargo clippy -p agent-types --all-targets --all-features -- -D warnings
```
