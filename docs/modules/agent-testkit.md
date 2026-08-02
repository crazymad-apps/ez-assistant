# agent-testkit 模块约束

## 模块定位

`crates/agent-testkit` 提供 Agent crate 共用的确定性测试能力。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md)、[`agent-model`](agent-model.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 核心约束

- 提供 scripted model、recorded transport、事件断言、取消 gate 和错误注入。
- 提供 scripted authorizer/policy，可观察 resolved arguments、批次大小和
  Policy → Authorizer → execute 顺序。
- 不访问真实 Provider、用户目录、应用数据库和 Tauri。
- 时序测试使用 channel/barrier/gate，不用不稳定 sleep 判断顺序。
- fixture 必须可审阅、可重放并移除 credential 和用户敏感内容。
- 不进入应用或生产 Adapter 的依赖闭包；产品 crate 只能通过 dev-dependency
  使用。本仓库唯一例外是独立开发工具 `tools/runtime-harness`：其离线 `verify`
  命令运行时需要 scripted 能力，因此可以使用普通 dependency。
- `examples/engine_demo.rs` 提供 v0.2.0 Agent Engine 离线演示，不访问真实
  Provider、文件系统或数据库。
- 可以直接依赖 `agent-context` 提供其确定性测试桩和断言辅助，但不得在 testkit
  复制窗口计算、历史布局或压缩算法。
- `FakeFileSystemTool` 只保存绝对逻辑路径，可注入稳定文件错误并观察取消；
  `FakeShellTool` 分别回放 stdout/stderr，可确定性结算退出、超时、I/O 失败、取消和
  输出截断。两者都不得访问真实文件系统或启动进程。
- `FakeFileSystemTool` 的目录只由文件路径隐式表达，不模拟空目录、符号链接、特殊文件
  或具体 OS errno；不能用它断言搜索不存在根、目录删除等真实 backend 边界的精确错误。

## 验证

```bash
cargo test -p agent-testkit
cargo clippy -p agent-testkit --all-targets --all-features -- -D warnings
cargo run -p agent-testkit --example engine_demo
```
