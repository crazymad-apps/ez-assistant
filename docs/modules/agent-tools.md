# agent-tools 模块约束

## 模块定位

`crates/agent-tools` 承载 Agent 工具 SPI、注册表、派发器、文件/Shell 能力契约与标准工具壳定义。修改前必须阅读 [`Agent 系统技术架构`](agent-system.md) 和 [`Rust 编程规范`](../specs/Rust编程规范.md)。涉及记忆工具时还必须阅读 [`agent-memory 模块约束`](agent-memory.md)。

本 crate 只定义工具能力、resolved invocation 与规范 Tool Call 的派发契约；标准
工具壳不绑定本地 I/O，本地、远程或测试能力实现均可由 Runtime 或其他宿主
在装配期注入并注册进 Registry。本版本的本地实现单独归属
`agent-tools-local`。

## 职责

- 类型化 Tool 抽象与类型擦除：输入经 serde 反序列化即校验，确定性参数在无副作用
  resolve 阶段冻结，JSON Schema 由 schemars 派生并允许工具实例补充实际默认值。
- `ToolRegistry`（构建期注册，重名拒绝；定义注册时冻结）与 `ToolSetSnapshot`（不可变，随 `ExecutionSpec` 进入执行）。
- 整批规范 Tool Call 无副作用解析：按原数量和顺序形成 Valid/Invalid item；Valid
  同时持有只读 resolved invocation 与不可公开、不可替换的一次性 executor。
- `ResolvedToolBatch` 可被 Send authorizer future 安全持有只读引用；可执行
  payload 仍保持 crate-private，只能由 Dispatcher 消费一次。
- 按批次位置一次性执行：重复执行或执行 Invalid item 形成绑定原 call ID 的内部
  合约错误；Dispatcher 不保留单阶段 `dispatch` 旁路。
- 为 Authorizer 和 Guardrail 暴露类型安全的工具元数据、授权事实与稳定指纹，不暴露
  可任意错配的裸类型擦除值。
- 文件与 Shell 能力契约（trait、输入输出类型、错误语义）。
- 标准工具壳：固定模型可见名称、输入、描述、默认值和 resolved 语义，
  执行时委托给装配的能力实现。
- 标准 Pinned Memory 与 Memory Recall 工具壳；领域类型和能力 trait 来自
  `agent-memory`，本 crate 不实现 Store 或 RecallSource。

## 核心约束

- 只依赖 `agent-types`、`agent-memory` 与已确认的通用第三方 crate；不依赖
  `agent-core`、具体 Store/RecallSource、Tokio 运行时句柄、Tauri 或 Runtime。
- 不使用 `async-trait`；异步 trait 方法手写 boxed future（沿 `ModelService` 先例）。
- 工具统一经类型化 `register` 注册，`input_schema` 固定由 schemars 从 `Input`
  派生（擦除层强制，不依赖实现方自觉）；类型擦除和一次性 resolved executor 是
  crate 内部机制，不提供可绕过 schema、facts 与执行载荷一致性的公开
  `register_erased` 入口。
- 会影响结果范围、时长或副作用的实际默认值必须写入注册时冻结的 Tool Definition；
  resolved 阶段采用的默认值必须与模型可见 schema 一致。
- 类型擦除执行器自己捕获原 call ID 并构造 `ToolResult`，类型化工具实现不能返回或
  替换 call ID；任何 Dispatcher 合约错误仍绑定原调用 ID，不能破坏 Call/Result
  配对。
- `ToolDefinition` 在注册时读取一次并与工具句柄共同冻结；重名检查、快照定义与名称索引都基于冻结定义。
- `ToolError` 只分 `InvalidInput`（校验/参数失败）与 `Execution`（执行失败）两类；
  Execution 可携带受控的结构化 details（例如 Shell 超时的已截断 stdout/stderr），
  两类都转为错误 `ToolResult` 回喂模型；取消不是 `ToolError`，由
  `ToolContext.cancellation` 观察处理；工具收到取消后必须完成资源清理再解析
  future。
- resolve 只允许类型校验、默认值落实、词法路径解析和纯授权事实生成，不得检查文件
  是否存在、解析符号链接、读取文件或启动进程。
- 能力契约只描述能力形状与错误语义，不含任何业务安全策略（权限、确认、审计归
  Runtime 与 Authorizer）。
- 本 crate 不访问 `std::fs`、`tokio::fs`，不启动子进程；具体能力实现由基础设施
  Adapter 提供，标准工具壳由 Runtime 或其他宿主装配。
- `pin_memory`、`update_pinned_memory`、`unpin_memory`、`list_pinned_memories` 和
  `recall_memory` 使用固定模型可见 Schema，通过普通 Registry/Dispatcher 路径工作；
  Source 列表不用于动态生成工具定义。
- Shell 契约：stdin 封闭、命令原样进入审计，显式区分 `managed` / `detached`
  生命周期；敏感环境变量默认不传给子进程；超时、输出上限、取消、输出收敛与进程树
  清理由实现侧负责。`detached` 只是 fire-and-forget 交接，不代表服务健康或受 Session
  托管。

## 文件与 Shell 能力契约

文件能力统一抽象为读取、列目录、内容搜索、写入、删除与局部编辑（edit）：

```rust
pub trait FileSystemTool: Send + Sync {
    fn read<'a>(
        &'a self,
        request: ReadFileRequest,
        context: FileToolContext,
    ) -> FsFuture<'a, ReadFileResult>;
    // list / search / write / delete / edit 同形态（FsFuture<'a, T> 为 boxed-future 别名）
}
```

- 模型输入中的相对路径基于显式 `session_workdir` 做纯词法归一化；能力请求、授权
  facts 和结果统一使用 UTF-8 `AbsolutePath`，不执行 canonicalize 或符号链接解析。
- read 的 offset/limit、search 的 max_results/output/record limit、edit 的
  replace_all，以及 Shell 的 workdir/timeout/output limit/process mode 都在 resolve 后
  成为显式有效值；Schema 同步公开模型可选参数的实际默认值。
- `grep`/`rg` 语义归入 `search`，契约不感知搜索后端，`rg` 仅可作为实现侧内部细节。
- `FileEntry.kind` 描述目标类型，`is_symlink` 独立标记目录项自身是否为符号链接。
- `FileToolError` 提供 `NotFound` / `InvalidInput` / `Io` / `UnsupportedEncoding` /
  `UnsupportedFileType` / `SearchBackendUnavailable` / `Cancelled` /
  `ConcurrentModification` / `TooLarge` 稳定分类。
- Shell 是独立一等能力，接受完整命令并发出流式 stdout/stderr；不按每条系统命令枚举专用工具，专用工具只在确实需要结构化领域结果时存在。
- 搜索达到结果数、stdout 总量或单记录上限时返回部分结果和稳定截断原因；能力契约不
  规定使用 `rg` 或其他具体后端。
- Shell 正常结果分别保留 stdout/stderr 和实际 process mode；非零退出码仍是正常结果，
  超时则形成带部分 stdout/stderr 的结构化执行错误。
- 结构化文件能力与 Shell 并存，不能用 Shell 取代文件能力。

## 不应放在本模块的内容

- Agent Loop、状态机、Recorder/Authorizer SPI、策略装配、Guardrail、执行事件与
  预算（归 `agent-core`）。
- 真实文件权限、Shell 确认交互、审计存储与进程管理实现（归 Runtime/Adapter）。
- 规范消息、Tool Call/Result 值类型（归 `agent-types`）。

## Harness 验证

- 单测自带 mini fake 能力实现，不 dev-depend `agent-testkit`（避免依赖环）。
- 覆盖：schema/default 校验、未知工具与参数/resolve 失败、整批保序、
  Registry 重名、定义冻结、facts 类型读取、fingerprint 和 executor 只消费一次。

```bash
cargo test -p agent-tools
cargo clippy -p agent-tools --all-targets --all-features -- -D warnings
```
