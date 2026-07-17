# agent-core 模块约束

## 模块定位

`crates/agent-core` 是与 UI 和应用生命周期无关的 Agent 执行引擎，负责一次 `AgentExecution` 内的模型调用、工具调用循环、上下文构建和执行事件。业务 Run 及其 `RunId` 属于 `assistant-runtime`。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 职责

- Agent Loop 与终止条件。
- 模型 Provider trait 和流式响应抽象。
- Tool trait、注册表和结构化调用协议。
- 单次 `AgentExecution` 的上下文组装、工具结果回填和事件输出。
- 与执行逻辑直接相关的 token、轮次和工具调用限制。
- 规范对话、Provider Codec、Memory/Safety/Recorder 等稳定能力接口。

## 核心约束

- Core 不知道 Tauri、窗口、会话列表、定时任务、SQLite 或配置文件位置。
- Core 不直接调用 `std::fs`、`tokio::fs`、`Command`、HTTP 工具或桌面 API；所有副作用通过注入的模型/工具 trait。
- Core 处理一个 `AgentExecution`，不持有 `RunId`，不负责跨会话排队和全局并发。
- `ExecutionPlan` 是已经由 Runtime 解析完成的不可变执行事实源；Core 不再维护 `AgentProfile`、配置默认值或覆盖顺序。
- 执行必须可取消，模型流和工具调用都要观察取消信号。
- 资源预算使用显式 `Option`；Core 不注入隐藏的最大轮次、超时或输出限制。
- 启发式 Guardrail 必须支持 `Off`、`Observe`、`Enforce`，不能以未声明规则静默中止执行。
- 工具输入先完成 schema/类型校验，再进入实现。
- Core 接收规范对话快照，不使用 `ConversationRef` 自行加载或持久化 Session。
- 规范对话记录与 UI/诊断事件分离；Provider 特有字段由 Codec 往返保真。

## 文件与 Shell 工具契约

文件能力统一抽象为：

```rust
#[async_trait]
pub trait FileSystemTool: Send + Sync {
    async fn read(&self, request: ReadFileRequest) -> Result<FileContent, FileToolError>;
    async fn list(&self, request: ListDirectoryRequest) -> Result<Vec<FileEntry>, FileToolError>;
    async fn search(&self, request: SearchFilesRequest) -> Result<Vec<SearchResult>, FileToolError>;
    async fn write(&self, request: WriteFileRequest) -> Result<WriteResult, FileToolError>;
    async fn delete(&self, request: DeleteFileRequest) -> Result<DeleteResult, FileToolError>;
}
```

- `grep`/`rg` 归入 `search`，Core 不感知搜索后端。
- Shell 是独立的 `ShellTool`，接受完整脚本并发出流式执行事件。
- 不枚举所有 Shell 命令为专用工具；专用工具只在确实需要结构化领域结果时存在。
- Core 通过注入的 Safety/Approval 服务完成决策编排；规则保存、用户交互、审计和真实执行约束由 Runtime/Adapter 完成。

## Memory 与 Provider 边界

- Core 只依赖稳定 `MemoryService`，不依赖本地数据库、远程协议或具体记忆算法。
- `MemoryPlugin` 由外部 Orchestrator 组合，支持本地、远程、混合、多来源和 No-op 实现。
- 记忆插件提供模型工具时走普通 Tool Contribution，禁止 Memory SPI 直接依赖 Tool SPI。
- Model Provider 一次只完成一个模型 Turn；工具继续循环属于 Agent Engine。
- reasoning、tool call、tool result 和 Provider continuation state 必须进入规范对话；不能只依赖流式事件恢复。

## 不应放在本模块的内容

- Session 列表、标题、归档和持久化。
- 全局 Run 队列、定时任务、模型账户配置。
- `RunId` 的生成、状态、查询、取消、恢复和事件关联。
- 真实文件权限、Shell 确认界面和审计存储。
- Tauri event/channel 与前端 DTO 转换。

## Harness 验证

- Agent Loop 使用 fake model 和 fake tool 覆盖：纯文本结束、工具调用、多轮工具、模型失败、工具失败和取消。
- 覆盖有限/无限预算与 Guardrail 三种模式，不假定固定最大轮次。
- 覆盖 Memory Plugin 聚合、Recorder 失败和 Provider reasoning/tool-call 往返保真。
- 事件顺序必须可断言，不依赖真实模型网络。

```bash
cargo test -p agent-core
cargo clippy -p agent-core --all-targets --all-features -- -D warnings
```
