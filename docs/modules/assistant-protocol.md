# assistant-protocol 模块约束

## 模块定位

`crates/assistant-protocol` 定义 UI 与 Assistant Runtime 之间，以及确有跨层消费者的稳定应用契约。Agent Core 内部的模型事件、规范对话、执行状态和能力 trait 不因复用方便而进入本 crate。

修改前必须阅读：

- [`Agent 系统技术架构`](agent-system.md)。
- [`Rust 编程规范`](../specs/Rust编程规范.md)。

## 允许内容

- `SessionId`、`MessageId`、`RunId` 等应用级 newtype ID；`RunId` 由 Runtime 持有，不进入 Agent Core 执行输入。
- Command/request、事件、快照和分页查询 DTO。
- 跨层共享的状态 enum、错误码和序列化约定。
- 文件与 Shell 工具的纯数据请求/响应类型。

## 禁止内容

- Tauri、Tokio task、数据库连接、Repository 实现。
- 模型 SDK、HTTP client、文件句柄、子进程句柄。
- UI 组件状态、窗口对象和展示文案。
- `Arc<Mutex<_>>`、trait object 或无法稳定序列化的运行时对象。
- 为单个模块内部实现服务的私有类型。
- Agent Core 的 `AgentExecution`、`ExecutionSpec`、`ModelEvent`、Provider 状态和 trait。

## 协议演进

- enum 序列化使用显式 tag，variant 名称视为兼容契约。
- 字段含义不能静默改变；需要改变时新增字段/variant并给出迁移路径。
- 新事件必须检查 Runtime 生产、Tauri 转发、前端消费、持久化和恢复。
- 可选字段必须区分“未知/未提供”和真实空值语义。
- 错误对外提供稳定代码和安全消息，内部错误链不直接序列化给 UI。

## v0.8.0 初始化契约

- `PROTOCOL_VERSION = 1` 只供 Runtime Host 私有握手比较应用契约版本，不表示 HTTP、WebSocket
  或其他传输协议。
- 公共 ID 包含 Session、Run、Message、Part 和 ToolCall；均为经过非空校验的透明字符串
  newtype，具体生成格式不构成契约。
- 公共命令只包含 create/list/get Session、start/get/cancel Run 和 shutdown；成功结果与事件使用
  显式 serde tag，失败使用稳定 `RuntimeErrorCode` 和脱敏消息。
- 公共快照只表达 Session 摘要以及 Run/Tool 的当前观察投影。完整规范 Conversation、pending
  exchange、Core/Provider 原始对象均不进入本 crate。
- request correlation ID、握手 envelope、Unix Socket、length-prefixed JSON frame 和 Demo 私有
  Conversation 查询属于 `apps/runtime-host`，不得因当前实现方便提升为公共 Protocol。
- 本版本不定义事件 sequence、replay cursor、持久化、Plan/Build、审批、Workspace、上传或记忆
  DTO。

## v0.9.0 配置与模型契约

- 项目仍处早期阶段，`PROTOCOL_VERSION` 保持 `1`，在当前 DTO 上直接演进；这里不存在所谓
  “V2 协议”或并行兼容代际。
- `ModelKey` 是用户配置 key，不是 Runtime 分配的 ID。创建 Session 可显式传入 key，未传时
  使用当前有效配置的默认 key；Session 摘要返回其冻结 key。
- 公共命令增加配置状态、模型列表/详情、显式 reload 和模型连接验证。所有配置投影均脱敏，
  credential 只能表示为 `api_key_configured`，不得进入响应、事件或错误。
- reload 的 Missing/Invalid/Degraded/Ready 是可诊断业务结果；连接验证返回稳定失败分类，
  原始 Provider body、认证 header 和内部错误链不进入协议。
- Runtime Home 文件路径可以作为本机诊断信息返回，但私有 wire、Demo 交互、模型 Profile 和
  Runtime 内部配置对象仍不属于公共协议。

## v0.10.0 可靠输入契约

- 项目仍处早期阶段，直接以 `SubmitInput`、`CancelQueuedInput`、`ResumeSession` 和 `RetryRun`
  替换旧 `StartRun` 提交入口，不保留双轨协议；`PROTOCOL_VERSION` 仍为 `1`。
- `IdempotencyKey` 是客户端生成的非空有界 opaque key，只表达同一 Session 内提交意图幂等；协议
  不定义正文摘要、全局去重或模型 credential 语义。
- `RunSnapshot` 通过 `input_id` 和 `attempt` 表达同一输入的多次执行尝试；`SessionSummary` 只公开
  `queued_input_count` 与 `resume_required` 投影，不公开 queue order 或存储表结构。
- `Accepted` 只表示输入与首次 Run 已持久化，不表示 Run 已经活动；只有 `RunStarted` 才表示该 Run
  被 Session 执行器领取。重启后的队列必须经显式 `ResumeSession` 才整体继续。
- `SessionLifecycle` 区分 active 与 archived；`ListSessions` 缺省只返回 active，并可显式筛选
  archived/all。归档 Session 的详情、Conversation 和 Run 历史仍可查询。
- 公共命令增加 `ListRuns`、归档/恢复、空闲模型切换与从历史 User Message 重新输入；历史重新输入
  只接受应用层 MessageId、新正文和可选幂等 key，不暴露 generation、文件路径或被删除表行。
- `SessionArchived` 与 `SessionNotIdle` 是不同的稳定错误：前者表示只读生命周期，后者表示活动、
  排队或未结算事实阻止当前变更。

## Harness 验证

- 序列化 round-trip。
- 关键事件 JSON 快照或明确兼容性测试。
- ID、状态转换和可选字段语义测试。

```bash
cargo test -p assistant-protocol
cargo clippy -p assistant-protocol --all-targets --all-features -- -D warnings
```
