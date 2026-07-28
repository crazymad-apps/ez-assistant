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
- Core 仍只负责一次 `AgentExecution`；Run/Session correlation 由本工具在外围
  附加，不传入 Core。
- 默认 `list`、`verify` 和测试完全离线，不读取 credential，不访问网络、用户文件、
  数据库或真实 Shell。
- 真实 Provider 只由显式 `chat` 模式启用；credential 只用于 service 构造，不进入
  请求 DTO、事件、错误、控制台或调试 payload。
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
cargo run -p runtime-harness -- chat
cargo run -p runtime-harness -- chat \
  --debug http://localhost:7331 \
  --debug-layer both
```

- `list` / `verify` / 默认测试不加载 `.env`，保持完全离线。
- `chat` 才加载 `.env` 并要求 `DEEPSEEK_API_KEY`；base URL 和模型分别由
  `DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL` 可选覆盖。
- chat 内支持 `/state`、`/reset`、`/cancel`、`/quit`；Ctrl-D 等价 `/quit`。
- 活动 Run 中普通文本与 `/reset` 明确拒绝；`/quit` 先取消并等待 Core 收敛和
  Journal 清理。
- `lookup_weather` 是固定返回演示数据的无副作用类型化工具，不访问真实天气服务。
- 调试前先在另一个终端运行 `cargo run -p debug-viewer`，然后打开
  `http://localhost:7331`。
- `--debug-layer provider` 只推送规范 `ModelRequest` / `ModelEvent`；
  `agent` 推送 `AgentEvent` 和 Run/Journal 事件；`both` 同时推送三路且为默认值。
- 同一 Run 的三路事件共享 `<session>/<run>` correlation 和全局递增 `seq`。
  viewer 的 Session 单选与右侧通道 Tab 只过滤页面展示，不限制 server 接收。
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
```
