# runtime-harness 模块约束

## 模块定位

`tools/runtime-harness` 是开发者显式启动的版本验证宿主，用于装配真实或 scripted
模型、Agent Engine、内存 Run/Journal 和 debug-viewer 推送，持续验证当前 checkout
对既有版本能力的兼容。

它是独立开发工具，不属于桌面产品进程、正式 `assistant-runtime`、sidecar、daemon
或本地服务。修改前必须阅读：

- [`Rust 编程规范`](../specs/Rust编程规范.md)
- [`Agent 系统技术架构`](agent-system.md)
- 当前版本功能设计、技术方案和开发计划

## 核心约束

- 产品 crate 不得依赖本工具；本工具只向下依赖 Agent crate、testkit 和
  debug-viewer。
- 内部 SessionId、RunId、状态机和 Journal 类型保持私有，不进入
  `assistant-protocol`，不定义正式 Runtime 产品语义。
- v0.3.0 直接装配 `agent-context` 的公共 Evaluator、Layout、Validator 和 Strategy；
  Checkpoint、任务链与 continuation 仅在本工具内以私有临时类型验证，不进入
  `assistant-runtime` 或 `assistant-protocol`，也不代表正式 Runtime 产品接口。
- Harness 可以持有临时压缩编排入口，但不得复制 `agent-context` 已有的窗口计算、
  历史切分、replacement 校验或 Rolling Summary 算法。
- Core 仍只负责一次 `AgentExecution`；Run/Session correlation 由本工具在外围
  附加，不传入 Core。
- 默认 `list`、`verify` 和测试完全离线，不读取 credential，不访问网络、用户文件、
  数据库或真实 Shell。
- 真实 Provider 只由显式 `chat` 模式启用；credential 只用于 service 构造，不进入
  请求 DTO、事件、错误、控制台或调试 payload。
- 真实 DeepSeek Chat 的正常 AgentExecution 显式冻结 reasoning 和
  `thinking: { type: enabled }`；调试观察装饰器只透传该配置，不负责注入或改写。
- 调试推送只建立到 debug-viewer 的出向连接；viewer 不可用不得影响执行。
- stdin 阻塞只允许在专用线程中；Tokio 执行线程不得被阻塞。
- 不为临时 Harness 提前实现正式 Runtime 的持久化、恢复、调度、多 Session 并发、
  权限策略或真实 FileSystem/Shell。

## 依赖例外

`agent-testkit` 原则上只进入测试依赖。runtime-harness 是唯一允许把它作为普通
dependency 的 workspace 开发工具，因为离线 `verify` 子命令在 binary 运行时需要
scripted model/tool/authorizer。该例外不得扩展到产品 crate。

## 运行入口

```bash
cargo run -p runtime-harness -- list
cargo run -p runtime-harness -- verify v0.2
cargo run -p runtime-harness -- verify v0.3
cargo run -p runtime-harness -- chat
cargo run -p runtime-harness -- chat \
  --debug http://localhost:7331 \
  --debug-layer both
```

- `list` / `verify` / 默认测试不加载 `.env`，保持完全离线。
- `chat` 才加载 `.env` 并要求 `DEEPSEEK_API_KEY`；base URL、模型和上下文窗口分别由
  `DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL`、`DEEPSEEK_CONTEXT_WINDOW_TOKENS`
  可选覆盖。上下文窗口缺省为 `128000`，必须是大于零的整数。
- Context 验证参数分别由 `HARNESS_COMPACTION_THRESHOLD_RATIO`（默认 `0.8`）、
  `HARNESS_SUMMARY_OUTPUT_TOKENS`（默认 `1024`）、
  `HARNESS_MINIMUM_RECENT_USER_TURNS`（默认 `1`）和
  `HARNESS_MAX_AUTOMATIC_COMPACTIONS`（默认 `2`）覆盖。比例必须位于 `(0, 1]`，
  摘要输出和自动压缩上限必须大于零，最少近期用户轮次允许为零。
- chat 内支持 `/state`、`/compact`、`/reset`、`/cancel`、`/quit`；Ctrl-D 等价
  `/quit`。空闲时 `/compact` 只执行主动压缩，不创建 Run、不续跑；活动任务链期间
  只按 Session 记录一个待执行请求，并在当前 Run/continuation 链结束后串行执行。
- 新用户消息先进入原始 Journal，再执行 Run 前窗口判断；阈值命中时先提交
  Checkpoint，随后才创建初始 Run。Core 返回 `CompactionRequired` 时，Harness
  结算当前 Run、压缩稳定历史并创建不重复 UserMessage 的 continuation Run。
- `/state` 同时显示原始消息角色、最新 Checkpoint 的 effective roles、Checkpoint
  数、当前任务链自动压缩次数/上限、主动压缩排队状态和最近压缩报告；默认不打印
  摘要正文。
- 活动 Run 中普通文本与 `/reset` 明确拒绝；`/quit` 先取消并等待 Core 收敛和
  Journal 清理。
- `lookup_weather` 是固定返回演示数据的无副作用类型化工具，不访问真实天气服务。
- 调试前先在另一个终端运行 `cargo run -p debug-viewer`，然后打开
  `http://localhost:7331`。
- `--debug-layer provider` 只推送规范 `ModelRequest` / `ModelEvent`；
  `agent` 推送 `AgentEvent` 和 Run/Journal 事件；`both` 同时推送三路且为默认值。
- 同一 Run 的三路事件共享 `<session>/<run>` correlation 和全局递增 `seq`。
  viewer 的 Session 单选与右侧通道 Tab 只过滤页面展示，不限制 server 接收。
- Run 外的主动 Context 维护使用 `<session>/context` correlation；Runtime 通道可读
  展示窗口判断、压缩完成、主动压缩排队和 continuation 关联，完整结构仍可展开。
- 用户输入成功写入 Journal 后，Runtime 发送一次 `user_message_appended`；它是用户
  消息的权威调试来源，不从每个 Provider `TurnRequested` 重复推导输入。
- `--debug` 优先于 `DEBUG_URL`；未配置 URL 时不创建 client 或模型观察 decorator。
  viewer 未启动或中途退出时仅关闭本 Run 的调试推送，不改变对话结果。

## 验证

M0 骨架阶段：

```bash
cargo metadata --format-version 1
cargo tree -p runtime-harness --depth 1
cargo check -p runtime-harness
cargo fmt --all --check
```

功能落地后还必须运行：

```bash
cargo test -p runtime-harness
cargo clippy -p runtime-harness --all-targets --all-features -- -D warnings
cargo run -p runtime-harness -- list
cargo run -p runtime-harness -- verify v0.2
cargo run -p runtime-harness -- verify v0.3
```
