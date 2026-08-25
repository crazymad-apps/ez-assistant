# Skills 机制讨论与 Agent 实现调研

- 记录日期：260821
- 状态：已纳入 `v0.19.0` 功能设计（本文继续作为输入记录）
- 来源：与用户的逐点讨论；对本地 Hermes Agent、OpenCode、Kimi Code、DeepSeek Reasonix、
  DeepSeek Harness 的只读实现调研；Agent Skills 官方规范
- 目标版本：`v0.19.0`（2026-08-22 已进入设计阶段）
- 正式设计入口：[`v0.19.0 功能设计`](../design/v0.19.0-功能设计.md)

## 一、原始目标

下一版本优先考虑 Skills，先调研其他 Agent 的 Skill 包格式、发现与注册、模型注入及 System
Context 机制，再确定适合 ez-assistant 的产品和 Runtime 语义。

讨论希望解决的核心问题包括：

1. Skill 包能否兼容通用 Agent Skills 格式；
2. 如何区分仅 Agent 调用、仅用户调用以及两者均可调用；
3. Workspace、用户和内置 Skill 同名时如何选择；
4. Skill Catalog 如何进入并更新模型上下文；
5. `load_skill` 加载正文后如何持久化、续跑并保持 UI 连贯；
6. Skill 文件更新由谁触发，是否自动观察文件系统变化。

本文是输入池中的讨论记录，已由 `v0.19.0` 功能设计吸收。本文本身仍不是已确认的
功能设计、技术方案或开发授权。

## 二、调研范围

本次读取了下列本地仓库的实际实现和文档：

| 项目 | 本地调研快照 | 主要关注点 |
| --- | --- | --- |
| Hermes Agent | `707f31668740` | 三级渐进加载、稳定 System Prompt 索引、`skill_view` 与用户 `/skill` 展开 |
| OpenCode | `7565e035` | Agent Skills 兼容目录、统一 Skill 状态、模型 Tool 与用户 Command 双入口 |
| Kimi Code | `c8029f9f` | 分层来源、冻结 Skill Listing、Tool Result 后 steer User Message |
| DeepSeek Reasonix | `77cf9aa7` | 固定索引上限、inline/subagent、启动期 System Prompt 注入 |
| DeepSeek Harness | `47f94385` | Provider Registry、独立调用策略、完整 Catalog replacement、动态上下文 |

通用格式依据：

- [Agent Skills Specification](https://agentskills.io/specification)
- [Adding Skills Support](https://agentskills.io/client-implementation/adding-skills-support)

## 三、外部实现结论

### 3.1 Skill 包格式

五个实现都以 Markdown 指令为主体，并普遍采用 `<skill-name>/SKILL.md` 目录包。Agent Skills
官方规范定义：

- 必填 `name`、`description`；
- 可选 `license`、`compatibility`、`metadata`；
- 实验性 `allowed-tools`；
- 可带 `scripts/`、`references/`、`assets/` 等支持文件；
- 启动或发现时只加载名称与描述，激活时加载完整 `SKILL.md`，资源按需读取。

OpenCode 的格式最接近最小通用规范；Hermes、Kimi 和 Reasonix 在 frontmatter 上增加了较多产品
专属能力。DeepSeek Harness 同时保留通用字段和客户端扩展字段。

### 3.2 调用方式

| 项目 | Agent 自主调用 | 用户显式调用 | 调用控制 |
| --- | --- | --- | --- |
| Hermes | `skill_view` 返回完整正文 | `/skill` 展开为 User Message | 无独立调用策略 |
| OpenCode | `skill` Tool Result 返回正文 | 每个 Skill 同时注册为用户 Command | 无独立调用策略 |
| Kimi | `Skill` 工具成功后 steer User Message | 用户 Skill 命令 | `disable-model-invocation` |
| Reasonix | `run_skill` / `read_skill` | `/<name>` | `invocation: auto/manual` |
| DeepSeek Harness | `skill` Tool Result 返回正文 | 识别直接用户 `/<name>` | `disable-model-invocation` + `user-invocable` |

Agent Skills 官方包规范没有定义调用策略；`disable-model-invocation` 属于已有客户端扩展，而不是
标准必选字段。

### 3.3 Catalog 与 System Context

- Hermes：名称和描述进入稳定 System Prompt 索引；正文由 Tool Result 或用户展开消息按需进入。
- OpenCode：每个模型 Step 重新组装当前 Skill Catalog 到 System Prompt，正文作为 Tool Result。
- Kimi：Skill Listing 冻结到 Session/Profile System Prompt；模型激活后追加 steer User Message。
- Reasonix：启动时把有长度上限的名称/描述索引固定进 System Prompt，正文按需返回。
- DeepSeek Harness：动态 Catalog 使用可持久的 User-role replacement context；正文由 Tool Result
  或用户显式注入进入历史。

ez-assistant 已有冻结 `SystemPromptSnapshot`、规范 Conversation Journal 和同一 Runtime Run 内的
上下文压缩 continuation，因此不宜每 Step 重建 System Prompt。动态 Skill Catalog 和 Skill 正文
应作为追加式规范消息进入 Conversation。

## 四、讨论收口

### 4.1 下一版本方向

- Skills 已于 2026-08-22 纳入 `v0.19.0` 功能设计；讨论稿建议暂不把 MCP、插件市场和
  远程 Skill 安装一起纳入，最终范围仍待确认。
- 本文继续作为输入池记录；当前版本状态、建议范围和新发现的待讨论项以
  [`v0.19.0 功能设计`](../design/v0.19.0-功能设计.md) 为准。
- 首版重点是本地 Skill 包、发现与优先级、Catalog 更新、用户/Agent 激活、持久化和自动续段。

### 4.2 包格式与兼容边界

首版倾向只支持规范目录包：

```text
<skill-root>/
└── review-pr/
    ├── SKILL.md
    ├── references/    # 可选
    ├── scripts/       # 可选，不自动执行
    ├── assets/        # 可选
    └── templates/     # 可选扩展目录
```

`SKILL.md` 采用 Agent Skills 标准字段，并兼容两个已有扩展：

```yaml
---
name: review-pr
description: Review pull requests and report actionable findings.
license: MIT
compatibility: Requires access to a source workspace.
metadata:
  author: example
disable-model-invocation: false
user-invocable: true
---
```

Runtime 内部不新增 `invocation: agent | user | both` 私有枚举格式，而是规范化为两个布尔事实：

```text
model_invocable
user_invocable
```

映射规则：

| 产品语义 | 包字段 |
| --- | --- |
| Agent、用户都可调用 | 两项省略 |
| 仅用户调用 | `disable-model-invocation: true` |
| 仅 Agent 调用 | `user-invocable: false` |
| 两者都不可调用 | 作为 disabled/诊断项，不进入任何调用目录 |

`allowed-tools` 可以解析和保留，但首版不得把它解释为越过 ez-assistant 权限系统的预授权。Skill
只提供指令，不授予文件、Shell、网络或其他工具权限。

首版明确不自动执行 `scripts/`，不注入环境变量或秘密，不支持 `runAs=subagent`、Skill 专属模型、
嵌套 Skill、Flow、在线市场或远程 URL 安装。

### 4.3 发现、注册与最近层级优先

同名 Skill 按离当前 Workspace/CWD 最近的层级优先过滤，不按不稳定的扫描或注册顺序覆盖。

候选方向为：

```text
最近的 Workspace Skill Root
→ 更远的 Workspace Skill Root
→ User canonical root
→ User .agents/skills
→ Bundled root
```

Workspace 内可同时考虑产品目录与兼容目录：

```text
.ez-assistant/skills
.agents/skills
```

约束：

- 最近层只覆盖同名 Skill，不丢弃远层的其他名称；
- 同层产品目录与兼容目录的精确优先级须在正式设计中固定；
- 完全相同优先级仍有冲突时不静默选择，应产生 ambiguous 诊断；
- 产品诊断应能说明 winner、shadowed candidate 和覆盖原因。

### 4.4 Catalog 初始注入与显式重新加载

Session 创建或首次建立 Skills 上下文时生成初始完整 Catalog。后续不依赖文件 watcher 自动发布
变化，由用户显式输入：

```text
/reload-skills
```

当前 Composer 尚无通用 Slash Command 机制。首版只需在提交入口识别这一条完整指令，不要求
先建设完整命令框架：

```text
trim(input) == "/reload-skills"
```

命中后不调用模型、不作为普通用户正文提交，而是调用 Runtime 的 Session Skills reload 意图。
Runtime 每次收到该意图都：

1. 重新扫描、校验并选择当前 winner；
2. 生成新的完整 Session Catalog Snapshot；
3. 无条件递增 revision；
4. 无条件向 Conversation Journal 追加完整 replacement；
5. 更新该 Session 后续 Run 使用的 Catalog binding。

不判断 Catalog 是否“真的变化”，也不生成 added/removed/updated diff。即使内容完全相同，也照样
追加新的完整 replacement。用户管理 Skill 文件，也由用户决定何时刷新。

Catalog replacement 倾向使用现有规范 `SystemMessage`，它是 Conversation 内追加的 System
Update，不重建 Session 的冻结 `SystemPromptSnapshot`，也不进入产品对话气泡：

```xml
<skill-catalog-replacement version="1" revision="7">
  <available-skills>
    ...
  </available-skills>
  <instruction>
    This complete catalog replaces every earlier skill catalog.
  </instruction>
</skill-catalog-replacement>
```

空目录也必须追加明确的空 replacement，以废弃历史 Skill 名称。文件摘要可以用于把一次
`load_skill` 绑定到已发布 Snapshot，但不得用于决定 reload 是否追加。

`/reload-skills` 在活动 Runtime Run 中的产品反馈和执行时机仍待正式设计。讨论中的倾向是接受
命令并在当前 Run 完整结束后自动应用，不能把新的 Catalog 插入正在使用冻结事实的
AgentExecution。

### 4.5 `load_skill` 的 Tool Batch 语义

`load_skill` 返回简短 Tool Result，不把正文放入 Tool Result：

```json
{
  "ok": true,
  "name": "review-pr",
  "status": "staged",
  "catalog_revision": 7,
  "message": "The skill becomes active after all tool calls in this response finish."
}
```

只要一个 Tool Batch 中包含 `load_skill`，该批次按模型声明顺序串行执行。不得跳过
`load_skill` 后面的其他工具：

```text
tool A
→ load_skill X
→ tool B
→ load_skill Y
→ tool C
→ 完整落账所有 ToolResult
→ 注入成功加载的 X、Y
→ continuation
```

Skill 正文不会影响当前 Assistant response 中同批的其他 Tool Call。工具说明必须突出：依赖 Skill
指令的动作不要和 `load_skill` 放在同一 Assistant response；应先单独加载，让后续执行段读取正文。

多个成功 `load_skill` 按原 Tool Call 顺序合并进一次持久化激活消息。至少一个成功加载时才发生
Skill continuation；如果全部失败，当前 AgentExecution 按普通错误 Tool Result 继续。该失败处理
属于当前讨论中的推导，正式设计时需再次核对。

### 4.6 Skill 正文持久化与同 Run 自动续段

Skill 正文采用 Kimi 式的“Tool Result 后注入 User Message”方向，但不在同一个
AgentExecution 内简单 steer。正文必须落入 Conversation Journal，并形成新的执行段边界。

关键术语：

- Runtime Run 是用户可见、可取消、可审计的业务执行，持有唯一 `RunId`；
- AgentExecution 是 Runtime Run 内部的一段 Core 执行，不拥有业务 ID；
- Skill 激活结束当前 AgentExecution，但不结束 Runtime Run；
- Runtime 自动启动下一段 AgentExecution，继续使用同一个 RunId，用户无需再次操作。

```text
Runtime Run r_123: Accepted → Running
│
├── AgentExecution segment 0
│   ├── Model Step 调用 load_skill
│   ├── 按序完成整个 Tool Batch
│   ├── 持久化全部 Tool Call/Result
│   └── 结束为通用 ContinuationRequired
│
├── 持久化 Runtime Hidden UserMessage
│   └── SKILL_ACTIVATION_V1 + 精确 Skill 正文
│
├── AgentExecution segment 1
│   └── 从包含 Skill 正文的新 ConversationSnapshot 自动继续
│
└── 最终结算 r_123: Completed / Failed / Cancelled
```

激活消息的预期语义：

```text
UserMessage
  origin = Runtime
  transcript_visibility = Hidden
  parts = [
    Injected(
      SKILL_ACTIVATION_V1,
      trigger = model,
      skill_name,
      catalog_revision,
      definition_digest,
      exact_skill_body
    )
  ]
```

正文一旦激活并持久化，后续重放不再依赖原始 `SKILL.md` 是否存在或发生变化。

该语义应复用现有上下文压缩 continuation：当前 Runtime 已能在同一个业务 Run 中结束一段
AgentExecution、更新 Conversation，再以相同 RunId 启动下一段。Core 不应知道 Skill 或
Runtime Run，只需返回 Provider-neutral 的 continuation/handoff 终态及本段预算消费；Runtime
读取可信的 Skill activation signal 并完成上下文变更。

UI 连贯性要求：

- 一个 Run 只发送一次 `RunAccepted`、一次 `RunStarted` 和最终一次 `RunFinished`；
- Skill continuation 中间不创建第二个 Run，也不显示新的用户气泡；
- Step 编号、usage 和预算在多个 AgentExecution segment 之间累计；
- 对外 Step 编号必须连续，不能因新 AgentExecution 从 1 计数而回退；
- 用户取消始终取消同一个 Runtime Run；
- 多次 Skill 激活需受单 Run 预算或显式次数上限约束，避免加载循环。

工具结果批次、Skill 激活消息和 continuation 必须具备可靠恢复语义，不能出现 Tool Result 已表示
`loaded=true`、但正文没有落库的半状态。具体是扩展 Recorder 原子批次、引入持久 staged signal，
还是由 Runtime Store settlement effect 统一提交，留待正式技术方案基于当前存储实现决定。

### 4.7 用户显式调用

用户显式调用的 Skill 在第一次模型请求前就应把正文作为该次规范 UserMessage 的 Injected Part
持久化，因此不需要先运行一个空的 `load_skill` 或触发 AgentExecution continuation。

用户入口尚未最终确认。候选包括：

- `/<skill-name>` 精确命令；
- Composer 的 Skill 选择器/上下文 Chip；
- 两者共用同一个 Runtime 用户激活意图。

无论入口为何，Runtime 都必须再次校验 `user_invocable`；模型 Tool 路径必须再次校验
`model_invocable`，不能只依赖 Catalog 或 UI 过滤。

## 五、建议的模块边界

以下是调研形成的方向，尚未成为正式技术方案：

- `assistant-runtime`：Skill 领域、来源优先级、Session Catalog binding、reload、调用策略、激活
  signal、同 Run continuation 和产品投影的权威者；
- Runtime Host：本地文件系统扫描、规范路径和 Skill 文件读取的具体 Adapter；
- `agent-core`：只理解通用 Tool Batch 和 continuation 终态，不依赖 Skill、Session 或 RunId；
- `agent-tools`：是否需要通用 post-tool delivery/control 契约，应以实际 `load_skill` 和现有压缩
  continuation 的接缝核查为准；
- `assistant-protocol`：只公开 Desktop 需要的 Skill 列表、诊断、用户意图和观察状态，不泄漏路径
  解析器、候选 locator 或持久化内部结构；
- Desktop：识别 `/reload-skills`、提供用户 Skill 激活入口并展示加载/诊断结果，不持有 Catalog
  权威状态。

首版不必为了未来远程 Provider 或插件市场建立 DeepSeek Harness 级别的公共 Provider Registry。
先使用职责清晰的 Runtime 私有 Catalog 和真实本地来源边界；出现第二种稳定来源后再评估公共 SPI。

## 六、安全与可靠性约束

1. Skill 是模型指令来源，不是权限授予来源；所有文件、Shell、网络和 MCP 工具继续经过现有
   Authorizer。
2. `allowed-tools` 不得绕过 Plan/Build、Session、Workspace、Global 权限与 Ask/Deny 规则。
3. `scripts/` 只作为按需引用资源，禁止发现或加载时自动执行。
4. Catalog 名称和描述、Skill 正文包装、路径和用户参数都必须使用确定性编码和转义。
5. Catalog 中不暴露绝对路径；仅在 Skill 成功激活后提供受控资源根或资源引用。
6. 当前 Run 使用已绑定的 Catalog revision；reload 不得静默改写活动 AgentExecution 的工具和上下文。
7. 同名覆盖、格式错误、不可读文件和过大正文必须形成可观察诊断，不得静默消失。
8. Skill 正文大小、单 Run 激活次数和支持资源读取仍需设置明确上限。

## 七、待正式设计确认

1. 已确认目标版本为 `v0.19.0`；本版本是否只包含本地 Skills，MCP/插件是否继续后置，
   仍需在功能设计中确认。
2. 产品 canonical Skill Root 的最终路径，以及同层 `.ez-assistant/skills` 与 `.agents/skills` 的
   精确优先级。
3. Session 初始 Catalog 是在创建 Session 时还是首次 Run 前持久化。
4. `/reload-skills` 在活动 Run 中的排队、取消和用户反馈语义。
5. Catalog replacement 最终采用 `SystemMessage` 还是具有专属来源的 Runtime Hidden
   UserMessage；需用所有 Provider Adapter 和压缩路径验证。
6. 用户显式激活 Skill 的最终入口和命令语法。
7. Core continuation 终态、Tool Batch 串行屏障和 Runtime activation signal 的最小契约。
8. Tool Result 与激活正文原子落账、崩溃恢复和重启后续段的存储方案。
9. 跨 AgentExecution segment 的 Step/usage/Guardrail/预算累计和事件编号。
10. Skill 正文与资源大小上限、加载循环上限、资源引用和文件变化后的错误语义。
11. 是否需要 Skill 启停配置；若需要，启停属于包字段、Session 设置还是 Workspace/User 配置。
12. 仅 Agent、仅用户、两者均可调用三种模式在 Desktop 列表中的可见性和说明。

## 八、明确暂不包含

- 在线 Skill 市场、Hub、Tap、远程 URL 和自动更新；
- Agent 自主创建、修改或删除 Skill；
- Skill 专属模型、推理强度、子 Agent Profile 或 `runAs=subagent`；
- Flow、参数模板、嵌套 Skill 和自动调度；
- 自动执行 Skill 脚本、环境变量透传和秘密注入；
- 以文件 watcher 自动向现有 Session 发布 Catalog 更新；
- 为尚不存在的远程来源或插件 ABI 提前建立通用 Provider Registry。
