# DeepSeek Responses API WebSearch 工具调用与事件植入讨论

- 记录日期：2026-08-28
- 状态：2026-10-23 用户确认产品内置资源随 Host 交付，首批 DeepSeek 搜索与 skill-creator，DeepSeek 不单独维护原生 JSON；功能方向与 F05 技术方案已确认，开发计划已确认，M0 已确认，M1 实现与验证完成待用户确认；用户再次明确 DeepSeek Search 默认不启用。取代以下旧方向。历史：2026-10-21 用户明确将当前 DeepSeek Search MCP 封装进通用辅助插件，Key 通过插件原生配置文件设置，插件不默认提供 Key；取代 10 月 20 日内置入口方向，下文旧讨论保留为历史。
- 来源：v0.20.0 会话；源于「给 Agent 终端接入 DeepSeek 服务端托管的 `web_search` 工具，并把其事件植入当前 Conversation 事件流投影到 UI」的需求
- 涉及组件：`agent-openai-compatible` / `agent-model` / `agent-types` / `agent-core` / `assistant-runtime` / `assistant-protocol` / `apps/desktop`

> 本文记录阶段性讨论结论，包含「官方调用规范」与「本地事件流植入方案」两部分。进入正式功能设计前需评审定稿。

## 2026-10-23 现行方向：产品内置资源

用户明确“不是将通用辅助直接合并进来，是维护一套内置插件、mcp、skill”，首批为 DeepSeek 搜索和 skill-creator，不将已有独立插件整包并入产品；旧源码与包只作复用参考。用户已确认随 Host 交付更新、插件 MCP 详情／环境变量及 Agent 配置方向，并明确 DeepSeek 不单独维护原生 JSON。[F05 功能设计](../versions/v0.32.1/features/F05-内置插件MCP与Skill/功能设计.md)已确认，[技术方案](../versions/v0.32.1/features/F05-内置插件MCP与Skill/技术方案.md)已确认，[开发计划](../versions/v0.32.1/features/F05-内置插件MCP与Skill/开发计划.md)已确认，M0 已获用户确认，M1 实现与验证完成、待用户确认；方案落实统一 MCP 用户配置、内置来源及同名规则，首批仅搜索，不自动纳入网页读取。DeepSeek Search 默认可见但禁用，保存 Key 不开启，升级保留用户启停意图。M0 已完成受管配置与生效后端，插件覆盖放原 config/mcp-overrides.json，内置覆盖使用用户域 mcp.json 的 managedServers.builtin；UI／Agent 管理入口已完成、待 M1 确认，实际内置资源留在 M2。F04 最终测试按用户指令后置，未记为已验收。

## 2026-10-21 历史方向：通用辅助插件与原生配置

用户明确指向 ez-assistant 当前 Agent 使用的 MCP，要求收录进通用辅助插件，
DeepSeek Key 通过原生配置文件设置，插件中不默认配置 Key。
已核对当前 MCP 为会话私有目录中的 Python stdio 实现，Host 已支持插件原生配置目录注入。
复用现有搜索／读取工具与 Host 契约，由独立插件仓库维护源码和随包运行时；
本仓库 F05 同步为关联交付，不新增内置服务预设，也不自动迁移既有 MCP 配置。

## 2026-10-20 历史方向：独立 Search／Fetch API 接入内置 MCP 渠道

用户在核对独立搜索接口后明确：集成到内置 MCP 渠道，一个服务一个 MCP，现阶段仅接 DeepSeek 测试；
默认不启用，用户手动配置并启用。该方向已纳入[v0.32.1 总体设计](../versions/v0.32.1/总体设计.md#35-内置插件mcp-与-skill)，
取代当前将模型托管搜索作为首期接入方案的建议，不意味着本版整个总体设计或开发计划已确认。

- DeepSeek MCP 独立暴露搜索和网页读取两个工具，复用现有 MCP 发现、调用、权限及会话记录机制；主模型可为其他服务商。
- 通过 `POST /v1/tools/search` 获取结构化搜索结果，按需通过 `POST /v1/tools/fetch` 获取 Markdown 正文。
- 内置仅提供入口与实现，默认禁用；用户配置自己的 Key，保存与启用分别执行。未启用不启动、不调用，缺少配置明确提示。
- 不合并其他搜索服务，不自动复用模型 Key 或企业共享凭据；本期不接 Responses 内置 `web_search`，不添加历史方案中的 `ToolCall.server_side` 标记。
- 当时独立插件分发是 10 月 19 日助手提出的候选；10 月 20 日以内置 MCP 渠道为准，后由 10 月 21 日用户指令替代。随包实现、配置表单与源码归属待 F05 方案核查，不假定当前已有内置服务预设机制。
- 参考 2026-10-19 已读取的官方[搜索／抓取指南](https://api-docs.deepseek.com/zh-cn/guides/search/)、[Search 接口](https://api-docs.deepseek.com/zh-cn/api/search/)和[Fetch 接口](https://api-docs.deepseek.com/zh-cn/api/fetch/)；实施前再核对契约，尚未用真实 Key 验证效果或稳定性。

以下第一、二部分保留原讨论事实及当时假设，其中模型范围、工具参数和建议实现均不作为当前已核验的实施依据。

---

## 第一部分：DeepSeek web_search 官方调用规范

### 1.1 结论：这不是 Chat Completions 的 Tool Calls，而是 Responses API 的服务端内置工具

- **接口**：Responses API（OpenAI Responses 兼容格式）
- **base_url**：`https://api.deepseek.com`
- **SDK**：`client.responses.create(...)`
- 支持模型：`deepseek-v4-flash` / `deepseek-v4-pro`

### 1.2 工具定义（tools）

```python
tools = [
    {"type": "web_search"}          # 亦可 {"type": "web_search_2025_08_26"}
]
```

关键点：
- `web_search` / `web_search_2025_08_26` 都是**服务端托管执行**的内置工具；
- `search_context_size`、`user_location` 参数**会被忽略**；
- 服务端自动续跑（search agent loop）**上限 10 轮**；
- 其它内置类型（`file_search`、`code_interpreter`、除 `apply_patch` 外的 `custom`）当前不支持。

### 1.3 工具选择（tool_choice）

| 取值 | 含义 |
| --- | --- |
| `auto` | 模型自行决定是否搜索 |
| `required` | 强制调用工具 |
| `{"type": "web_search"}` | 强制进行网页搜索（`tools` 里必须含 `web_search`，否则 400） |

### 1.4 输出项（output item）

模型中可能返回 `web_search_call` 类型的输出项，`action` 取值：
- `search` / `open_page` / `find_in_page`

流式 SSE 事件推进：
```
response.web_search_call.in_progress → .searching → .completed
```

多轮回填（pass-back）：`web_search_call` 按原样传回即可，服务端自动恢复搜索结果。

### 1.5 两个工具类型的区别

`web_search` 与 `web_search_2025_08_26` 在官方文档中**行为一致**（均服务端执行、忽略上述参数、10 轮上限）。区别仅按命名惯例推断：
- `web_search`：跟随最新版；
- `web_search_2025_08_26`：锁定到 2025-08-26 快照版本，用于可复现/防回归。

> 官方未明文列出二者功能差异，以上为基于命名惯例的推断，选型应以实际验证为准。

### 1.6 服务端为何需要「自动续跑多轮」

一次调用并非单轮 LLM 生成，而是服务端内部的 search agent loop：模型「搜索 → 读结果 → 重新决策 → 再搜」直到收敛给出最终答案，因此设置 10 轮上限避免递归/成本失控。这个「多轮循环」只属于服务端托管的 `web_search` / `web_search_2025_08_26`；其它 `function` 或本地工具由客户端控制循环。

---

## 第二部分：把 web_search 事件植入当前 Conversation 事件流

### 2.1 现有工具事件链路（现状）

```
Provider SSE 事件
  → agent-openai-compatible/src/responses/stream.rs  (ResponsesAssembler::push)
  → ModelEvent (agent-model/event.rs： ToolCallStarted/Delta/Finished)
  → agent-core/src/engine.rs::stream_turn  (消费 ModelEvent)
  → AgentEvent (工具执行活动)
  → assistant-runtime → RuntimeEvent (assistant-protocol/event.rs: ToolProposed/Started/Output/Completed)
  → apps/desktop UI
```

当前约定：**本地工具**由模型产出 `ToolCall` part → 引擎本地执行 → 逐条冒泡 `ToolStarted/Output/Completed` 给 UI。

### 2.2 现有代码对 web_search 的硬拦截点

| 层 | 文件 | 拦截点 |
| --- | --- | --- |
| 请求结构 | `responses/schema.rs:86` | `ResponsesFunctionTool` 只支持 `function`，无 `web_search` 工具类型；`tool_choice` 无 web_search 命名分支 |
| 事件解码 | `responses/stream.rs:131` | `response.web_search_call.*` → `Err("unsupported Responses event type")` |
| 输出项 | `responses/stream.rs:213`、`698` | `web_search_call` → `Err("unsupported Responses output item type")` |
| 输出项枚举 | `responses/stream.rs:36` | `OutputItemKind` 无 `WebSearch` 变体 |
| 事件模型 | `agent-model/event.rs:20` | `ModelEvent` 无服务端工具概念；`ToolCall*` 语义是「本地可执行」 |
| 会话片段 | `agent-types/conversation.rs:304` | `AssistantPart` 只有 `ToolCall`，无法表达「服务端已执行」的搜索 |
| 引擎 | `agent-core/engine.rs:143`、`180` | `tool_calls_of` + `Dispatcher::resolve_batch` 会把它当本地工具解析，未注册 `web_search` → 直接失败 |
| 协议事件 | `assistant-protocol/event.rs:67` | 复用即可，不必新增变体 |

### 2.3 讨论结论：复用现有 RuntimeEvent，不必新增事件变体

- 协议层 `ToolProposed / ToolStarted / ToolCompleted`（`event.rs:67` 起）**可以直接承接**：
  - 把 `web_search_call` 的 item `id` 当 `call_id`、`tool_name = "web_search"`，`ToolCompleted.status` 给 `ToolActivityStatus`（`Proposed/Running/Completed/Failed`）；
  - UI 据此渲染一张「web search」活动卡片。
- **唯一的坑**：不能把它作为普通 `AssistantPart::ToolCall` 扔给引擎走本地执行（本地未注册 `web_search`，`Dispatcher::resolve_batch` 会 unresolved 失败）。需要在「会话片段 + 引擎」这层打一个小补丁。

### 2.4 推荐的最小部署方案（MVP）

给 `AssistantPart::ToolCall` 增加一个 **`server_side: bool`（服务端已执行）标记**，引擎对这类调用：

- **复用现有 RuntimeEvent**：只冒泡 `ToolProposed` → `ToolCompleted`；
- **跳过**本地 `resolve_batch` / `execute_batch` / `ToolOutput`（本地工具专有的 stdout/stderr 通道，服务端搜索本就没有）；
- 状态映射：`in_progress/searching → Running`，`completed → Completed`；
- **只发终态**：解码到 `completed` 才冒泡，中间态不逐条透传（配合「裁掉流式」的取舍，改动最小）。

链路：
```
web_search_call (provider)
  → 解码成带 server_side 标记的 ToolCall
  → 引擎：ToolProposed → ToolCompleted（复用 RuntimeEvent，零新增变体）
  → UI：web_search 活动卡片
```

### 2.5 已权衡的取舍

| 取舍点 | 当前默认 | 说明/风险 |
| --- | --- | --- |
| 事件粒度 | **只发终态** | 放弃 `in_progress/searching` 进度动画，换来改动最小 |
| Part 形态 | 复用 `ToolCall` + `server_side` 标记 | 改动小；语义上区分本地/服务端，比新增独立 Part 更省 |
| 是否走审批 | 服务端工具默认不拦截 | 服务端已执行，审批意义需斟酌 |
| 结果是否入库 | 暂不保留 provider 原形 | 若后续轮次要复用搜索结果（pass-back），provider 期望 `web_search_call` 原样结构，需为此额外挂 `OpaqueProviderState`，本阶段先不做 |

### 2.6 后续可选项

- 补流式 `in_progress/searching` 事件，搜索结果保留为可 pass-back 的 provider 原生数据；
- 若产品 UI 需要，可将 web_search 从「工具活动」卡片升级为独立展示形态。

---

## 附录：待确认

1. 是否按 MVP（复用 RuntimeEvent + `server_side` 标记 + 只发终态）落地；
2. `server_side` 标记的字段命名与序列化边界（避免破坏既有请求编码与 round-trip）；
3. 是否需要为服务端搜索保留独立的 Approval/Guardrail 策略。
