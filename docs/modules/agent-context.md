# agent-context 模块约束

## 模块定位

`crates/agent-context` 提供 Provider-neutral 的共享模型上下文能力。v0.3.0 中
`agent-core` 和 `assistant-runtime` 是正式调用方，`runtime-harness` 是直接装配其余能力的
临时验证调用方。它负责统一规则和可替换策略，不持有应用会话或业务 Run。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- 通过唯一 Context Window Evaluator，基于最近完整 Provider Result 的
  `total_tokens` 和当前 `ModelService::context_window_tokens()` 判断窗口占用。
- 按完整 User Turn 和 Tool Exchange 构造历史布局，区分 protected System prefix、
  可压缩 head 与 protected recent tail。
- 复用 `agent-types` 的规范对话结构校验，并校验压缩候选 replacement。
- 定义 Compression Strategy 边界；v0.3.0 只实现使用当前 ModelService 的
  Rolling Summary。

## 依赖与所有权

- 只直接依赖 `agent-model`、`agent-types` 和 workspace 已有的通用异步/序列化依赖。
- 不依赖 `agent-core`、`assistant-runtime`、Provider Adapter、`assistant-protocol`、
  Tauri、数据库或 debug-viewer。
- 不定义或持有 SessionId、RunId、Context Checkpoint、Store、任务链恢复次数和
  continuation。
- 不按 provider/model 名称猜测上下文窗口，不读取配置文件或 credential。
- 不执行工具，不驱动 Agent Loop，不发布 Runtime 事件。

## 核心约束

- Core、Harness 和未来 Runtime 不得在本 crate 外重复窗口比例、历史切分或 replacement
  校验逻辑。
- usage 缺失时返回明确判断结果，不在业务代码中回退到裸算或隐藏 TokenEstimator。
- 窗口判断在最新 Provider `total_tokens` 上额外计入当前 Conversation 中 ProviderState payload 的
  保守字节预算；该预算用于避免本地不透明续传包绕过窗口限制，不把 payload 当 reasoning 正文。
- 压缩策略只生成候选或 NoOp 报告，不提交 Checkpoint，也不决定是否续跑。
- `RollingSummarySameModel` 只调用一次 `CompactionInput` 中的当前 ModelService；
  请求原样保留正常 Agent 的完整冻结 System Prompt，并由 Strategy 内部在
  `protected prefix + compressible head` 后追加临时摘要指令；调用方不拼接请求
  conversation，recent tail 不发送给压缩模型。
- 临时摘要指令使用 request-only `InternalContext` 计划，只存在于 compression request，不写入原始
  History、Layout、Candidate 或 Checkpoint。
- compression request 使用空 tools、`ToolChoice::None` 和显式输出上限。
- 只有 `FinishReason::Stop`、无 ToolCall 且至少包含一段非空 Text 的完整响应才能形成
  Candidate；Reasoning 不进入摘要正文，取消、Overflow 和普通模型错误原样受控返回。
- Candidate 固定为 protected System prefix、最新 ContextSummary 和 recent tail；recent tail
  必须原样保留。ContextSummary 的 `compacted_usage` 只累计实际被摘要替换的 head，不能提前
  吸收仍在 recent tail 中的模型调用用量。
- `ContextLayout::build` 只做共享结构校验和原子分块；Rolling Summary 的
  `minimum_recent_user_turns` 只在 `partition` 时参与 head/tail 边界计算。
- `ContextLayout` 不按 `TranscriptVisibility` 过滤消息；隐藏 Runtime User Message 与可见 User
  Message 使用同一 user role 和 Turn 结构进入模型上下文，产品转录过滤属于 Runtime/Host 投影职责。
- 每条规范 `UserMessage` 都开始一个 User Turn；可见用户输入、隐藏 Runtime continuation 与内部
  插入消息遵循同一分块规则，不按来源、可见性或内部类别额外保留。
- 当前 Runtime 统一要求 replacement 只原样保留最近一个 User Turn，之前的摘要和所有更早
  Turn 都进入可压缩 head。单一 User Turn 自身撑满上下文时不拆分该 Turn，也不合成 continuation
  锚点；轮内压缩作为后续版本的独立设计事项处理。
- Tool Call/Result、reasoning、ProviderState 和消息顺序是不可破坏的协议正确性边界；但历史中
  出现 ProviderState 本身不构成永久 protected tail，完整旧 User Turn 可进入压缩 head，摘要替代后
  其 opaque state 自然离开活动上下文。
- 所有比例和策略配置在构造期校验；运行时错误使用稳定错误类型，不泄露 prompt、
  credential 或完整响应正文。
- 不引入 Passthrough Strategy；预算内请求由 Core 直接构造既有 `ModelRequest`。
- Tool Result 裁剪在 v0.3.0 只保留 Strategy 扩展入口，不实现消息类型或算法。

## 验证

```bash
cargo test -p agent-context
cargo clippy -p agent-context --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
