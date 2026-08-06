# ez-assistant

本地优先的桌面 AI 助手。正式 Assistant Runtime 作为独立产品进程承载业务权威状态与 Agent
能力，Tauri 桌面端通过本地应用协议作为它的 UI 客户端；仓库中的各类 Demo 是独立开发验证
宿主，不属于产品进程。

## 仓库结构

```text
apps/
  desktop/                         Tauri 2 + Vanilla TypeScript 桌面应用
  runtime-host/                    正式 Assistant Runtime 产品进程入口
crates/
  agent-types/                     Provider-neutral 规范消息与工具值类型
  agent-model/                     单次模型 Turn、事件、错误与有限建立重试
  agent-provider-openai-compatible/ OpenAI-compatible Codec、Transport 与服务 Adapter
  agent-context/                   窗口判断、历史布局、校验与压缩策略
  agent-memory/                    Pinned Memory 与 RecallSource 契约
  agent-tools/                     工具 SPI、Registry/Dispatcher、能力契约与标准工具壳
  agent-tools-local/               真实本地文件与 Shell Adapter
  agent-core/                      单次 AgentExecution 推理与工具循环
  agent-sdk/                       会话级冻结能力的候选薄 Facade
  agent-testkit/                   确定性模型、工具、Recorder 与测试支持
  assistant-protocol/              跨应用层共享 DTO
  assistant-runtime/               产品会话、Run、调度、配置与持久化编排
tools/
  debug-viewer/                    独立调试事件查看器
  runtime-harness/                 Core 与 Context 累计验证宿主
  safety-demo/                     安全策略与真实本地工具验证宿主
  memory-demo/                     记忆系统验证宿主
  reliability-demo/                Trace、重试与回放验证宿主
  core-demo/                       v0.7.0 B/S 综合验证宿主
```

核心依赖方向是：

```text
desktop → assistant-protocol ← runtime-host → assistant-runtime → agent-sdk → agent-core
                                                                  ├────────→ agent-tools → agent-types
                                                                  ├────────→ agent-context → agent-model
                                                                  └────────→ agent-model

core-demo → agent-sdk → agent-core
```

具体 Provider、本地文件/Shell 和应用持久化由顶层宿主装配，不进入 `agent-sdk` 或
`agent-core`。

## Agent SDK

`agent-sdk` 是普通宿主的候选入口。一个 `Agent` 保存一个会话内冻结的模型、System Prompt、
Context Window、ToolSet 和执行配置，但不保存动态 Conversation、Run 或 Journal。

```rust
use std::sync::Arc;
use agent_sdk::{AgentBuilder, AllowAllAuthorizer};

let agent = AgentBuilder::new(model, system_prompt, context_window)
    .tools(tools)
    .model_request(model_request)
    .build()?;

let execution = agent.start_ephemeral(
    input,
    cancellation,
    Arc::new(AllowAllAuthorizer),
);
let outcome = execution.completion.await;
```

以上代码中的模型、输入和授权策略仍由宿主显式提供。可直接运行两个完全离线的例子：

```bash
cargo run -p agent-sdk --example minimal
cargo run -p agent-sdk --example custom_controls
```

普通使用、Runtime 风格 Recorder、扩展 SPI 与高级 Adapter API 的边界见
[`Agent SDK 导读`](docs/Agent-SDK导读.md)。
当前主机的确定性性能结果、平台/Provider 能力矩阵和验收证据见
[`v0.7.0 兼容性与性能基线`](docs/versions/v0.7.0/兼容性与性能基线.md)。

当前可以启动使用真实 DeepSeek、真实本地文件/Shell、Demo 私有安全策略、JSON Memory
Store/RecallSource 和上下文交接的 Core Demo。启动前在仓库 `.env` 或进程环境中提供
`DEEPSEEK_API_KEY`；可选覆盖 `DEEPSEEK_BASE_URL`、`DEEPSEEK_MODEL` 和
`DEEPSEEK_CONTEXT_WINDOW_TOKENS`：

```bash
cargo run -p core-demo -- serve \
  --workdir . \
  --data-dir /tmp/ez-assistant-core-demo \
  --port 0 \
  --max-compaction-handoffs 2 \
  --retry-transient
```

服务只绑定 `127.0.0.1`，终端会输出 OS 分配的实际页面地址。页面接受自然语言任务，由模型
自主调用标准工具，便于检查 Plan/Build × Ask/Auto、审批、审计、Pinned Memory 会话冻结、
`recall_memory` 来源/部分失败和上下文交接。`--retry-transient` 只开启建流前有限重试；不传即
关闭。Pinned Store 与 Recall 样例位于指定 data-dir；Session 与 Journal 只存在于当前进程，
页面关闭或刷新不会取消后台 Run，但服务退出后不会恢复会话。Shell 以当前用户权限运行，
workdir 不是沙盒，data-dir 也不与 Agent 文件或 Shell 工具隔离；请使用专用验证目录，并显式
停止 detached 进程。

不访问网络、用户数据文件或 Agent 工具的 Core 候选性能基线可单独运行：

```bash
cargo run -p core-demo --release --example core_baseline
```

## Runtime Host 初始化 Demo

v0.8.0 的正式 Runtime Host 通过 Unix Domain Socket 提供私有验证入口。先设置 Provider credential，
分别在两个终端启动 Host 与 feature-gated Demo：

```bash
DEEPSEEK_API_KEY=... cargo run -p assistant-runtime-host -- serve
cargo run -p assistant-runtime-host --features demo-client -- demo
```

Demo 启动后自动列出 Session，并在空 Runtime 中创建第一个 Session。使用 `Tab` 切换会话列表与
消息输入，`Enter` 发送，`Ctrl+C` 取消活动 Run，`R` 手动重连；会话列表聚焦时按 `Q` 只退出
客户端，`Ctrl+Q` 经二次确认后受控关闭 Runtime Host。初始化版本只暴露无外部副作用的
`echo_text`，Session、Run 和 Journal 仍仅保存在当前进程内；它不监听 HTTP/TCP，也不是系统
daemon。

## 启动桌面应用

```bash
cd apps/desktop
npm install
npm run tauri dev
```

## 开发与检查

开发前从 [`AGENTS.md`](AGENTS.md) 开始阅读，并按改动范围继续阅读 `docs/specs/` 和
`docs/modules/` 下的约束。长期架构见
[`Agent 系统技术架构`](docs/modules/agent-system.md)，版本路线见
[`版本管理`](docs/版本管理.md)。

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
