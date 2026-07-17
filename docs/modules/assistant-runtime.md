# assistant-runtime 模块约束

## 模块定位

`crates/assistant-runtime` 是应用业务运行时和权威状态所有者。第一阶段直接运行在 Tauri Rust 进程内，不是 sidecar 或本地服务。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- Session、Message、Run 的生命周期和查询。
- 多会话并发、同会话串行、全局模型/工具限流。
- Agent Core 实例管理和事件持久化/广播。
- 将配置、Agent 模板、用户偏好和会话覆盖编译成唯一不可变的 `ExecutionPlan`。
- 为每个 Runtime Run 创建绑定 `RunId`/`SessionId` 的 Recorder、Memory、Approval 和 Event Adapter。
- 定时任务、配置加载、后台任务与恢复。
- 文件、Shell 等工具的真实实现、授权、确认与审计。
- Repository/持久化边界和应用级错误。

## 并发模型

```text
Tauri 进程
└── Tokio Runtime
    ├── Session A：Run 串行
    ├── Session B：Run 串行
    ├── Scheduler
    └── Tool/Shell tasks（全局限流）
```

- 不同 Session 可以并发；同一 Session 默认只有一个改变上下文的 Run。
- 历史 Session 不绑定永久线程或永久任务；有 Run/定时事件时才激活。
- 每个 Run 有 `RunId`、状态、取消令牌和最终落盘结果。
- `RunId` 不传入 Agent Core；Runtime 将独立 `AgentExecution` 的输出关联回对应 Run。
- 客户端窗口销毁或事件订阅断开不得自动取消 Run。
- 广播事件前后要有明确持久化策略，保证 UI 重建后能以快照恢复；不能依赖 WebView 永久在线。
- 模型、工具、Shell 和阻塞任务分别设置并发上限。

## 文件能力

- `FileSystemTool` 的 Runtime 实现负责真实路径、授权根、符号链接策略、大小限制、确认和审计。
- `search` 可以调用打包的 `rg` 或 Rust 搜索后端，必须使用参数数组启动程序，不通过 `sh -c` 拼接查询。
- 写入优先采用同目录临时文件和原子替换；覆盖、删除等策略由权限模式决定。
- 文件授权只约束结构化文件工具，不能宣称约束 Shell。

## Shell 能力

- Shell 是通用工具，不维护不可持续的完整命令白名单。
- 支持 `Disabled`、逐次确认、会话信任、工作区信任和完全信任等策略；工作区信任不是 OS 沙盒。
- 执行记录包含原始脚本、cwd、开始/结束时间、退出码和确认依据。
- stdout/stderr 流式输出并限制总量；支持超时、取消和进程树清理。
- 默认不向子进程传递模型 API Key、应用令牌等敏感环境变量。
- 风险分析只提供提示，不能作为强安全边界。

## 配置与持久化

- 配置来源、默认值、覆盖顺序和校验必须显式。
- 配置合并结果只生成一份 `ExecutionPlan`；Core 不再二次解释 Profile、默认值或覆盖关系。
- 会话、消息、Run、定时任务和审计记录由 Repository trait 隔离具体存储。
- Conversation Journal 是规范对话的权威状态；流式 `AgentEvent` 不是消息恢复来源。
- Runtime 提供绑定当前 Run 的 `ExecutionRecorder`，但其接口不向 Core 暴露 `RunId`、事务或数据库实体。
- 数据库写入错误不能伪装成成功事件；需要定义运行结果与持久化失败的处理策略。
- 数据库 schema 变更遵循根 `AGENTS.md` 的数据库安全规则，并提供迁移与回滚说明。

## 不应放在本模块的内容

- WebView DOM 状态、窗口位置、悬浮球视觉状态。
- Tauri command 宏和具体窗口句柄。
- 具体 UI 文案和确认弹窗实现。
- 与单次 Agent Loop 内部算法强相关但不涉及应用调度的逻辑。
- Provider 消息编解码、规范工具分发、上下文压缩算法和启发式 Guardrail 实现。

## Harness 验证

- 覆盖不同 Session 并发、同 Session 串行、取消、排队、订阅断开、重启恢复和 Scheduler 补跑策略。
- 文件和 Shell 测试使用临时目录与无破坏命令，不访问用户真实文件。

```bash
cargo test -p assistant-runtime
cargo clippy -p assistant-runtime --all-targets --all-features -- -D warnings
```
