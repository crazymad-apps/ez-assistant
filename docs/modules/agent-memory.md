# agent-memory 模块约束

## 模块定位

`crates/agent-memory` 承载与存储、Runtime 和模型工具无关的记忆领域与能力契约。修改前
必须阅读 [`Agent 系统技术架构`](agent-system.md) 和
[`Rust 编程规范`](../specs/Rust编程规范.md)。

Pinned Memory 与 Memory Recall 是两个独立子域：前者在创建会话时渲染为冻结的 System
Prompt Part，后者由普通模型工具按需调用。Agent Core 不增加 Memory 专用执行阶段。

## 职责

- Pinned Memory 的标识、归类、正文、Runtime 字符串/数字业务属性、变更输入和显式容量限制。
- `PinnedMemoryStore` 替换边界及稳定错误分类，不绑定本地文件或数据库。
- 将结构化 Pinned Memory 确定性渲染为安全转义的 System Prompt Part。
- Memory Recall 的检索请求、稳定引用续读请求、结果、来源和失败类型。
- `RecallSource` 数据源检索适配边界、`MemoryRecall` 多来源检索调用边界，以及仅由有序数据源可选实现的
  `RecallReferenceReader` 稳定引用续读边界。
- 可确定测试的多 Source 协调实现，包括显式默认集合、并发、超时、取消、精确去重、
  截断和部分失败。

## 依赖与所有权

- 只直接依赖 `agent-types` 和 workspace 已确认的通用异步、序列化、错误及 XML 依赖。
- 不依赖 `agent-core`、`agent-tools`、`assistant-runtime`、Provider Adapter、
  `assistant-protocol`、Tauri 或应用代码。
- 不访问本地文件、数据库、网络服务、配置、credential 或用户目录。
- 不提供 `agent-memory-local`；具体 Store 和 RecallSource 由 Runtime、应用或独立宿主实现。

## 核心约束

- 异步 trait 使用手写 boxed future，不引入 `async-trait`。
- Pinned Memory 限制由调用方完整传入；超限明确失败，不提供隐藏默认或自动淘汰。
- 快照渲染必须排序确定、正确转义且受最终字节上限约束；相同输入产生相同输出。
- `attributes` 仅供 Runtime 业务与持久化使用，不是正文的结构化副本；快照渲染必须忽略它，
  模型可见工具投影也不得暴露它。
- 最终会话快照只保存渲染完成的字符串，不提供从成品快照反解析领域条目的 API。
- `RecallSource` 只声明稳定 Source ID 和检索能力，不持有 Prompt、Skill、Session、权限或
  模型可见性规则。
- `MemoryRecall` 只承诺多来源搜索；`before/after/around` 依赖稳定顺序与引用验证，只能通过可选的
  `RecallReferenceReader` 提供。不得为追求单一接口迫使普通 Source 返回“不支持续读”。
- 所有召回结果天然携带来源；单个 Source 失败可以与其他有效结果同时返回。
- 协调实现不进行语义合并、跨 Source score 归一、自动写入或 Pinned Memory 修改。
- 本 crate 不定义模型可见工具；标准工具壳归 `agent-tools`。

## 不应放在本模块的内容

- JSON、SQLite、向量库或远程 API Adapter。
- Session 创建、恢复、分支、Journal 和 System Prompt 总体组装。
- Source 可见性提示、Skill 注入、授权、审批和审计。
- 自动观察、提炼、整合、候选记忆、淘汰或后台学习任务。

## 验证

```bash
cargo test -p agent-memory
cargo clippy -p agent-memory --all-targets --all-features -- -D warnings
cargo fmt --all --check
```
