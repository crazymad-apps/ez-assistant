# Workspace 固定标签与多目录方案

- 记录日期：2026-08-28
- 状态：提案（待确认，进入正式设计前需评审）
- 来源：v0.20.0 会话；源于「`list_managed_sessions` 只能拿到不透明 `workspace_id`，无法区分哪个 Workspace 对应哪个项目」的问题
- 目标方向：为 Workspace 增加**固定可读标签（Title/Label）**，并支持**一个 Workspace 下挂多个目录**；同时让 Agent 能通过工具感知该关联。

> 本文属于进入正式功能设计前的**阶段性讨论与方案提案**，不提前敲定表结构、字段类型、迁移脚本等技术细节。已确认业务语义后再进入
> [`versions`](../versions/) 下对应版本的功能设计与技术方案。

---

## 1. 背景与动机

当前 Runtime 在数据层已经保存了「Session ↔ Workspace」的关联，但该关联对 Agent 而言是**不可读的**，导致实际使用中出错。典型例证：

- 通过 `list_managed_sessions` 能拿到每个会话的 `workspace_id`，但 `workspace_id` 是不透明字符串（如 `w_BrulO6BQ2sGn`）；
- Agent 无法判断 `w_BrulO6BQ2sGn` 究竟是「宋金战争」还是「EasyAssistant」，于是出现委托到错误会话的问题；
- `WorkspaceSummary` 没有 name/label 字段，唯一带语义的 `user_directory`（如 `/Users/chenjiale/github/ez-assistant`）没有下发给 Agent，只在存储层可见。

## 2. 用户明确的业务诉求

1. **工作空间需要一个固定的 Title 或 Label**：一个稳定、可读、长期不变的工作空间名称，能让人（和 Agent）一眼认出「这是哪个项目/工作区」。
2. **一个工作空间下可以有若干个不同的目录**：即 Workspace 是「若干根目录/工作目录」的集合，而不仅仅是当前模型里的单个 `user_directory`。

需要说明的是，「固定」意味着该标签不随某个 Session 的标题变化、不随会话内容自动滚动，是 Workspace 自身的稳定元数据。

## 3. 现状分析（代码定位）

### 3.1 关联的存储位置

- `crates/assistant-runtime/src/environment.rs` — `SessionExecutionEnvironment { workspace_id: Option<WorkspaceId>, ... }`
  - **Session → Workspace 的绑定在创建时冻结进 `environment.workspace_id`**，这是权威关联来源；一个 Session 最多绑定一个 Workspace。
- `crates/assistant-runtime/src/storage/workspace.rs` — `StoredWorkspace { workspace_id, user_directory, agent_directory, lifecycle, ... }`
  - 一个 Workspace 目前对应**一条 canonicalize 后的用户工作目录**（`user_directory`），并带一个 Runtime Home 下的 `agent_directory`。

### 3.2 暴露给上层/Agent 的形式

- `crates/assistant-runtime/src/runtime/controller/mod.rs` — `ManagedSession { session_id, title, workspace_id, proxy_enabled, can_accept_message }`
  - **Agent 可直接调用的 `list_managed_sessions` 只返回不透明 `workspace_id`**，没有目录、没有标签。
- `crates/assistant-protocol/src/snapshot.rs` — `SessionSummary { ..., workspace_id: Option<WorkspaceId>, ... }`
  - `WorkspaceSummary { workspace_id, user_directory, agent_directory, lifecycle, created_at_ms, updated_at_ms, removed_at_ms }`
  - **`WorkspaceSummary` 无 name/label**；`SessionSummary` 也仅暴露 `workspace_id`。
- `crates/assistant-protocol/src/command.rs` — `RegisterWorkspaceRequest { path }` / `ListWorkspacesResult { workspaces: Vec<WorkspaceSummary> }`
  - 协议层已有 `ListWorkspaces` / `GetWorkspace` 等命令，但：
    - `RegisterWorkspaceRequest` **只能传一个 `path`**，无法给 Workspace 命名；
    - `WorkspaceSummary` 不带 label，因此即便协议层列出来，Agent 仍无法识别项目归属。

### 3.3 结论

> 关联在**存储层已成立**，但 `workspace_id` 对 Agent 是无意义句柄；**唯一带语义的 `user_directory` 与（尚不存在的）`label` 都没有进入 Agent 可见的工具输出**。要让 Agent「知道」Workspace 与 Session 的关联，本质是补齐「workspace_id ↔ 可读标签/目录」的可见性，而不是新增关联本身。

## 4. 方案设计

### 4.1 为 Workspace 增加固定 Title/Label

- 新增 Workspace 持久化字段 `title`（或 `label`，命名待定），语义为「固定、用户可设置的工作空间名称」。
- 默认值可取自首个/主目录的 basename（如 `ez-assistant`），但一旦设定即为用户显式元数据，**不随 Session 标题、会话内容或自动生成机制滚动**。
- 登记/恢复入口：在 `RegisterWorkspaceRequest` 增加可选 `label`；或在登记后提供独立的重命名命令（`RenameWorkspaceRequest`），支持修改。
- `WorkspaceSummary` 增加该字段，供产品 UI 与 Agent 统一消费。

### 4.2 一个 Workspace 支持多个目录

- 将 Workspace 从「单 `user_directory`」演进为「**有序的多根目录集合**」。
- 每个目录条目建议具备：
  - 逻辑名（展示用，可与顶层目录名一致）；
  - canonicalize 后的绝对路径；
  - 目录角色/作用域（是否计入权限、是否只读等）。
- 需要明确多目录化对以下能力的影响：
  - Agent 的文件读写作用域（由单目录扩展为多根目录的并集）；
  - `PermissionFileScope::Workspace` 的匹配语义；
  - `workspace_private_directory` 是否仍为 Workspace 级单一私有目录，还是随目录扩展；
  - Skills / 指令自动注入的来源目录集合。

### 4.3 让 Agent 感知关联（关键落地）

在不改变既有关联的前提下，把「workspace_id ↔ 标签/目录」暴露给 Agent。两条可并行但不冲突的路径：

1. **扩展 `list_managed_sessions`**（`controller/mod.rs`）：为 `ManagedSession` 增加
   - `workspace_label: Option<String>`
   - `workspace_directories: Option<Vec<String>>`
   - Active 从 Session 的 `environment.workspace_id` 反查 Workspace 的 `title` 与目录列表。
   - 这是最小改动、不动协议，能立刻解决「不知道哪个是哪个项目」。
2. **新增/强化 Agent 工具**：例如为 Agent 提供 `list_workspaces` 工具，返回 `Vec<WorkspaceSummary>`（含 `title` + 目录），让 Agent 自行建立 `workspace_id ↔ 项目` 映射，再与每个 Session 的 `workspace_id` 对上。

> 推荐优先级：先落地第 1 条（成本最低、见效最快），再视产品 UI 需求推进第 2 条与 `WorkspaceSummary.title` 的协议级扩展。

### 4.4 兼容性

- 存量 Session 的 `workspace_id` 不变，`SessionExecutionEnvironment` 结构不破坏。
- 存量「单目录」Workspace 迁移为「单元素的目录集合」，`user_directory` 可保留为兼容字段或作为第一目录。
- 未绑定 Workspace 的普通会话保持 `workspace_id = None` 不受影响。

## 5. 待确认的开放问题

- Workspace 字段命名：`title` / `label` / `name`，以及是否需要区分「用户手设」与「自动派生」。
- 多目录时 Agent 文件作用域的确切边界，以及"是否允许跨目录写"等策略。
- `workspace_private_directory` 与多目录的关系。
- 目录表是否与 Git 仓库/子仓库解耦（例如一个 Workspace 同时包含多个 repo / submodule 目录）。
- 标签是否允许重名；重名时 Agent 如何区分。
- 多目录与权限文档（`PermissionFileScope::Workspace`）的匹配粒度是否需要细化。

## 6. 总结与下一步

- **目标明确**：给 Workspace 补一个固定可读标签，并支持其下挂多个目录。
- **本质**：补齐 `workspace_id ↔ 标签/目录` 在 Agent 侧的可见性，而非新建关联。
- **落地顺序建议**：
  1. `list_managed_sessions` 带出 `workspace_label` + `workspace_directories`（改 Agent 工具，不动协议）；
  2. `WorkspaceSummary` 增加 `title`，`RegisterWorkspaceRequest` 支持命名/独立改名（协议 + 产品 UI）；
  3. 多目录作为下一阶段，在功能设计阶段明确目录集合模型与文件作用域策略。

本文为提案，需进入对应版本的正式设计与技术方案评审后再敲定字段、表结构与迁移细节。
