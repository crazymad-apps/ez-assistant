# Rust 编程规范

本规范适用于 workspace 内所有 Rust crate。编码前还必须阅读对应 `docs/modules/*.md`。

## 一、总体原则

- 先搜后写，优先复用已有类型、trait、错误和测试工具。
- 显式代码优先；重要状态转换、权限判断和副作用不能只隐藏在宏或隐式生命周期中。
- 归属优先于类别：模块私有类型就近放置，稳定跨模块契约才进入 `assistant-protocol`。
- 依赖倒置用于真实边界，不为每个结构体机械创建 trait。
- 默认禁止 `unsafe`；workspace 已配置 `unsafe_code = "forbid"`。

## 二、crate 职责与依赖

- `agent-core`：单次 Agent 执行、模型和工具抽象；不得依赖 Tauri、数据库和桌面 API。
- `assistant-runtime`：应用状态、会话、Run、调度、配置、持久化与工具实现编排。
- `assistant-protocol`：共享 DTO、事件、ID 和稳定枚举；保持依赖最少。
- `ez-assistant-desktop`：Tauri adapter 和原生桌面能力；不承载 Agent 业务流程。

新增依赖前检查：

1. 标准库或现有依赖能否满足。
2. 依赖应属于哪个 crate。
3. 是否引入原生库、运行时或跨平台打包成本。
4. 是否影响许可证、二进制体积和最低系统版本。

多个 crate 共用的第三方依赖版本应提升到根 `Cargo.toml` 的 `[workspace.dependencies]`。

## 三、模块与命名

- 模块和文件使用 `snake_case`，类型和 trait 使用 `UpperCamelCase`，函数和变量使用 `snake_case`，常量使用 `SCREAMING_SNAKE_CASE`。
- 类型名表达领域含义：`SessionId`、`RunHandle`、`ShellRequest`，避免 `Manager2`、`Util`、`CommonData`。
- Command 表达意图，Event 表达已发生事实；例如 `StartRun` 与 `RunStarted`。
- 布尔值使用 `is_`、`has_`、`can_`、`should_` 等可读前缀。
- 单文件膨胀时按领域职责拆分，避免 `utils.rs` 和 `common.rs` 成为杂物箱。

## 四、类型与错误

- ID 使用 newtype，不在业务层传播裸 `String` 或整数。
- 跨层时间、路径、状态和错误语义必须明确，不以无结构 `serde_json::Value` 代替稳定模型。
- 库代码返回具体错误类型，应用边界再转换为可展示错误；错误链保留 source。
- `panic!`、`unwrap()`、`expect()` 只允许用于测试或编译期确定的不变量；运行时输入、文件、网络、模型和数据库错误必须传播或处理。
- 不用布尔值同时表达结果和失败原因，使用 enum 或 `Result`。
- 对外集合避免暴露可变内部状态，优先返回拥有值、只读引用或流。

## 五、异步、并发与取消

- 网络、模型流、文件异步 I/O 和调度使用 Tokio；不得在异步任务中直接执行长时间阻塞工作。
- CPU 密集或阻塞库调用使用 `spawn_blocking` 或专用执行器，并设置并发上限。
- 不同会话可并发；同一会话的上下文变更 Run 默认串行。
- 每个 Runtime Run 和 Shell 执行必须有稳定 ID、状态、取消句柄和结束事件；Agent Core 的一次 `AgentExecution` 不持有业务 `RunId`，由 Runtime 在外围完成关联。
- 后台任务不得持有 WebView 生命周期对象；窗口销毁不能导致 Runtime 状态丢失。
- 全局并发使用 `Semaphore` 等显式限流，禁止无界创建模型请求、工具任务或通道。
- 通道必须说明背压策略；除非能证明事件可安全丢弃，否则不要使用无界通道。
- 持有同步锁期间不得 `.await`；锁粒度应小，不能跨模型、文件、Shell 或数据库调用。

## 六、Agent 工具

- 工具接口使用结构化请求/响应和稳定错误，不把任意 JSON 或命令字符串混入非 Shell 工具。
- 文件搜索属于 `FileSystemTool::search`；其内部可以选择 `rg` 或 Rust 实现。
- Agent Core 只依赖工具 trait，不直接执行文件系统或子进程副作用。
- Shell 使用独立 `ShellTool`；完整脚本必须原样进入审计和确认，不在展示时截断或静默改写。
- Shell 默认不继承应用持有的 API Key、令牌和其他敏感环境变量。
- Shell 支持 stdout/stderr 流、超时、取消、输出上限和进程树清理；第一阶段不默认支持交互式 TTY。

## 七、序列化与协议

- UI 与 Runtime 之间的稳定 DTO 放在 `assistant-protocol`，使用可演进的显式字段；Agent Core 内部模型、对话、执行状态和能力接口不进入应用协议。
- 事件 enum 使用稳定 tag；新增 variant 前检查前端 exhaustive handling、持久化和恢复逻辑。
- 不直接序列化内部锁、句柄、trait object 或数据库实体。
- 路径在 Rust 内使用 `PathBuf`；跨 WebView 边界时明确编码与平台语义。
- 协议发生不兼容变化时必须记录迁移策略，不通过悄悄修改字段含义完成复用。

## 八、配置、日志与敏感信息

- 配置有默认值、来源优先级和校验错误；启动时尽早失败并给出可操作信息。
- API Key 等凭据不得写入普通配置、日志、Shell 环境或错误展示。
- 日志使用结构化字段记录 `session_id`、`run_id`、`tool_call_id` 等关联标识。
- 不记录完整模型提示、文件内容和 Shell 输出，除非用户明确开启调试且了解风险。
- 可恢复错误记录上下文后返回；禁止吞错或只打印错误仍返回成功。

## 九、测试

- 纯状态转换、协议和权限判断优先单元测试。
- Runtime 编排使用 fake model/tool/repository 验证事件顺序、取消、并发和失败恢复。
- 文件测试使用临时目录，不读写用户真实目录。
- Shell 测试使用无破坏性的固定命令，并验证超时、取消、stdout/stderr 和输出截断。
- 并发测试不能依赖不稳定 sleep 判断顺序，优先使用 barrier、channel 或可控时钟。

最低检查：

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```
