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
  `max_input_tokens()` 仅保留为模型参数，不增加自动压缩阈值。
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
- usage 缺失时返回明确判断结果，不在业务代码中回退到整段历史裸算或隐藏 Tokenizer。
  独立输入限制同样不代表可精确预估首次请求或新增文本、图片、工具结果；这类实际 Token 校验仍由
  Provider 执行。禁止为本地预检静默截断内容；窗口判断不是新的 Token 计数服务。
- 窗口判断以最新 Provider `total_tokens` 为权威基数，只对该响应之后、尚未得到 Provider usage 的
  User/Tool 文本做显式轻量增量估算；ASCII 按 4 字符/token、非 ASCII 按 1 字符/token，并计入固定
  消息/Part 开销。ProviderState 已包含在产生它的响应 `total_tokens` 中，不得再按密文字节数重复计费。
  `ContextTokenUsage` 分开保存 `completed_tokens` 终态和 `pending_tokens` 临时估算，合计值供 Runtime
  与 Evaluator 共用；新完整响应替换基数并清除已计量的临时增量，即使真实用量比旧估算低也接受。
  Evaluator 不联网；图片等本地无法可靠计量的内容由既有 Provider Overflow 兜底。
- Context Summary 可以携带显式 usage adjustment：`subtract` 从摘要后仍保留的 Assistant usage
  中减去压缩边界前最后一次已计量总量，并补入统一渲染后的摘要轻量估算；若无法建立可靠边界则
  标记 `unavailable`，不得借用旧 usage。replacement 中没有旧 Assistant usage 时不写 adjustment，
  首次预检自然保持 usage unavailable；新的完整 Assistant Result 到达后重新成为权威基数。
- 压缩策略只生成候选或 NoOp 报告，不提交 Checkpoint，也不决定是否续跑。
- `RollingSummarySameModel` 只调用一次 `CompactionInput` 中的当前 ModelService；
  Strategy 克隆 Runtime/Core 交来的完整正常 `ModelRequest`，在其完整 conversation 末尾追加临时摘要
  指令。System、原 conversation、工具定义及顺序、generation 其余字段、reasoning 和 Provider Options
  均保持不变，只把 `tool_choice` 改为 `None` 并覆盖摘要输出上限。
- 临时摘要指令使用 request-only `InternalContext` 计划，只存在于 compression request，不写入原始
  History、Layout、Candidate 或 Checkpoint。
- 正常请求含工具时，当前模型必须显式支持 `ToolChoice::None`；否则在模型调用前失败，不能清空工具
  定义来伪造另一份请求。
- 无 ToolCall 且至少包含一段非空 Text 的完整响应才能形成
  Candidate；Reasoning 不进入摘要正文，取消、Overflow 和普通模型错误原样受控返回。
- Candidate 的紧凑 JSON 字节数必须小于压缩前权威快照，否则以 ineffective 错误拒绝提交；该效果
  校验是共享入口，Runtime 在补入宿主私有的程序化上下文后必须再次调用，不能只依赖策略初次检查。
- Candidate 固定为 protected System prefix、最新 ContextSummary 和 recent tail；recent tail
  必须原样保留。ContextSummary 的 `compacted_usage` 只累计实际被摘要替换的 head，不能提前
  吸收仍在 recent tail 中的模型调用用量。
- `ContextLayout::build` 只做共享结构校验和原子分块；Rolling Summary 的
  `minimum_recent_user_turns` 只在 `partition` 时参与 head/tail 边界计算。
- `ContextLayout` 不按 `TranscriptVisibility` 过滤消息；隐藏 Runtime User Message 与可见 User
  Message 使用同一 user role 和 Turn 结构进入模型上下文，产品转录过滤属于 Runtime/Host 投影职责。
- 每条规范 `UserMessage` 都开始一个 User Turn；可见用户输入、隐藏 Runtime continuation 与内部
  插入消息遵循同一分块规则，不按来源、可见性或内部类别额外保留。
- 完整轮次分区优先；没有完整旧轮次可回收时，允许在当前可见任务 User 后的闭合工具交换之间切分。至少保留最新完整交换及后续内容，原始 User 通过 retained_user_message_id 引用保留；不得截断 pending 批次、生成别名或重复产品消息。隐藏 Runtime User 不替代任务锚点。
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

## v0.30.0 F01 M1 计量与机制守卫

现有 usage 基线加增量估算保留，Evaluator 明确标为 estimated；无基线为 unknown。`context_compaction` 消息的 usage 是压缩调用费用，不能替换正常响应的窗口基线，也不按 ProviderState 密文字节估算。可选远端计量由模型/宿主调用，Evaluator 仍不联网。

RollingSummarySameModel 在发起模型调用前核对冻结策略，仅允许本地机制；原生策略不落回摘要。M2 已补齐轮内安全回收、User 引用式候选和压缩范围指令；扣除 head、补入摘要的既有估算保持。原生策略的真实执行继续按后续里程碑落实。

## v0.30.0 F01 当前执行边界

Core 按冻结机制构造有效输入：原生使用最新已提交完整窗口及后缀，不以模型、入口地址或临时续接周期丢弃窗口，本地使用摘要投影；两者均先通过共享完整交换校验。压缩后的一次软阈值续跑许可不绕过已知硬限制、不重置 Run 预算；无正常模型进展时不无限压缩。精确计量只由当前有效请求的可靠样本提供，输入变化立即失效；失败沿用旧方案并等待正常响应校准。
