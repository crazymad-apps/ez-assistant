# MCP 机制讨论与 Agent 实现调研

- 记录日期：260821
- 状态：已纳入 `v0.23.0`
- 来源：与用户的逐点讨论；对本地 OpenCode、Hermes Agent、Kimi Code、DeepSeek Harness
  的只读实现调研；MCP 协议机制
- 目标版本：`v0.23.0`

> 2026-09-02 后续讨论已将首版收敛为：按 MCP Server 业务范围分级发现、固定
> `discover_mcp_tools` / `call_mcp_tool` 两个网关、完整定义使用普通 Tool Result、
> `/mcp refresh` 手动刷新，以及 stdio 与 Streamable HTTP 同版交付。本文第四节及后续保留早期
> 调研和候选推演，不再代表当前设计结论；正式范围以
> [`v0.23.0 功能设计`](../versions/v0.23.0/功能设计.md)为准。

## 一、原始目标

讨论 MCP 在 Agent 产品中的核心机制，以及 LLM 如何调用 MCP、如何感知 MCP Server 提供的工具。
重点澄清以下问题：

1. MCP Client、Agent Runtime 与 LLM 的职责如何分开；
2. `tools/list` 得到的全量工具目录是否应冻结进 System Prompt；
3. 大量 MCP Tool Schema 如何避免挤占模型上下文；
4. OpenCode、Hermes、Kimi Code 等 Agent 采用全量暴露还是动态加载；
5. ez-assistant 是否应像 Skills 一样采用渐进式披露；
6. MCP 工具加载、上下文持久化、同 Run 自动续段和实际调用应采用什么语义。

本文是独立的 MCP 输入记录，不与 Skills 机制文档合并，也不是已经确认的功能设计、技术方案或
开发授权。

## 二、MCP 的核心机制

### 2.1 MCP 不直接连接 LLM

LLM 不建立 MCP 连接，也不直接发送 `tools/list` 或 `tools/call`。产品中的基本链路是：

```text
MCP Server
    ⇅ MCP transport / protocol
Assistant Runtime（MCP Client）
    ⇅ ModelRequest / ModelEvent
LLM Provider
```

Runtime 负责：

1. 从配置发现并连接 MCP Server；
2. 完成 MCP 初始化和能力协商；
3. 调用 `tools/list` 获取工具名称、描述和输入 Schema；
4. 把允许模型感知的能力投影为模型上下文或模型原生工具；
5. 接收模型产生的工具调用意图；
6. 完成参数校验、权限判断、审批和审计；
7. 调用 MCP `tools/call`；
8. 把结果转换为规范 Tool Result 并继续 Agent Loop。

MCP 只规定 Client 与 Server 之间的发现和调用协议，不规定 Agent 必须如何把工具披露给模型。
全量原生 Tool List、搜索桥接、动态加载和 Code Mode 都是 Agent Runtime 的产品策略。

### 2.2 四种不同的“工具列表”

讨论中必须区分：

```text
Live MCP Catalog
    MCP 连接当前实际提供的完整目录，可随连接和通知变化

Run Catalog Binding
    某个 Runtime Run 允许访问的目录 revision、可见性和稳定身份

Model-visible ToolSet
    当前模型请求原生 tools 字段中真正出现的工具

Conversation Disclosure
    通过规范消息告诉模型的工具名称、描述和完整 Schema
```

把某个 Run 绑定到 Catalog revision，不等于把完整 Catalog 写入 `SystemPromptSnapshot`，也不等于
必须把全部工具 Schema 放进 Provider 的 `tools` 字段。

Provider 的原生 `tools` 通常是模型请求中的独立字段，不属于 System Prompt；但 Provider 仍需读取
其中的名称、描述和 JSON Schema，因此依然占用上下文、影响输入成本和 Prompt Cache。仅仅把工具
从 System Prompt 移到 `tools` 字段，不能解决目录膨胀。

### 2.3 Catalog 动态变化

MCP Server 可以声明工具列表变化能力，并通过工具列表变化通知要求 Client 重新执行 `tools/list`。
连接建立、断开和失败同样会改变 Live Catalog。

Live Catalog 可以动态刷新，但是否立刻改变正在执行的 Agent 上下文是另一项产品决策。讨论倾向：

- Runtime MCP 连接管理器维护动态 Live Catalog；
- 每个 Run 绑定明确的 Catalog revision 和可见范围；
- 当前 Run 不静默替换已经披露的工具定义；
- Live Catalog 更新默认影响后续 Run；
- Server 在当前 Run 中断开时，调用返回明确的暂不可用错误，不把它伪装成未知工具。

具体是否提供 MCP 手动刷新指令、活动 Run 中如何处理列表更新，留待正式设计确认。

## 三、外部 Agent 实现调研

本次读取了下列本地仓库：

| 项目 | 本地调研快照 | 主要机制 |
| --- | --- | --- |
| OpenCode | `7565e03536d1` | 默认全量原生工具；实验性 Code Mode 使用单一入口和预算化搜索 |
| Hermes Agent | `707f31668740` | MCP/插件工具默认由 search/describe/call 三个桥接工具渐进披露 |
| Kimi Code | `c8029f9f3bbd` | 实验性 `select_tools` 与消息级动态 Tool Schema |
| DeepSeek Harness | `47f943859bef` | Native/Code/Both 三种呈现；Code Mode 生成完整工具 SDK |

### 3.1 OpenCode

OpenCode 默认模式不是冻结一次完整 MCP Tool List，而是在每个模型 Step 调用
`SessionTools.resolve`，读取当前 `mcp.tools()` 并把所有可见 MCP 工具转换为模型原生工具。
MCP Server 的文字说明可以进入 System Prompt，但工具 Schema 通过模型请求的 `tools` 字段发送。

OpenCode 监听 MCP 工具列表变化通知，收到通知后重新拉取 Server Catalog；后续 Step 重新组装工具
时即可看到更新。因此默认模式具有动态刷新能力，但所有可见 Schema 仍会产生每次请求的上下文成本。

实验性 Code Mode 的行为不同：

- 不再把每个 MCP 工具注册为模型原生工具；
- 只向模型提供一个 `execute` 工具；
- MCP 工具在受限代码运行时中按 Server 分组；
- `execute` 描述只内联预算允许的目录，默认 Catalog 预算约 2,000 Token；
- 未内联的工具通过 `tools.$codemode.search` 搜索后调用。

因此 OpenCode 同时包含“默认全量原生工具”和“实验性渐进披露”两条路径。

### 3.2 Hermes Agent

Hermes 当前实现把 MCP 和非核心插件工具替换为三个固定桥接工具：

```text
tool_search
tool_describe
tool_call
```

核心 Hermes 工具继续直接暴露。只要存在可延迟的 MCP/插件工具，默认 `auto` 配置就启用桥接；完整
Schema 不进入模型原生 Tool List。

Hermes 的目录呈现按预算降级：

1. 预算足够时显示名称和简短描述；
2. 超限时退化为仅名称；
3. 极大目录只显示 Server 名称和工具数量；
4. 具体工具通过 `tool_search` 搜索；
5. `tool_describe` 返回完整参数 Schema；
6. `tool_call` 使用通用 `{name, arguments}` 调用实际工具。

目录从当前 Registry 重新构建，不以 Session 内存缓存作为权威来源。桥接调用会解包为实际工具名，
使既有 Guardrail、插件 Hook、审批和结果截断仍按实际工具执行。

Hermes 是当前调研中最接近通用 Provider、固定工具网关方案的实现。

### 3.3 Kimi Code

Kimi Code 的实验性 `tool-select` 需要同时满足：

- 模型支持 Tool Use；
- 模型声明 `dynamically_loaded_tools`，即消息级工具定义能力；
- 实验开关启用。

启用后：

1. 顶层 `tools[]` 保持为核心工具与 `select_tools`；
2. Runtime 在 Turn 边界以 `<tools_added>` / `<tools_removed>` 告诉模型可加载名称；
3. 模型调用 `select_tools` 选择精确名称；
4. Runtime 把完整工具定义追加为持久化 `role: system` 消息的 `tools` 字段；
5. Agent Loop 在下一 Step 重新读取可执行工具表，所选工具立即可调用；
6. Context Compaction 会丢弃消息级 Schema，之后需要重新选择。

该方案是真正的动态 Tool Schema 加载，但依赖 Kimi 模型的消息级工具定义扩展，不能作为普通
OpenAI-compatible Provider 的通用底层契约。

### 3.4 DeepSeek Harness

DeepSeek Harness 的工具呈现支持：

- `native`：每个可见工具作为原生模型工具；
- `code`：模型原生工具只保留 `run_code`；
- `both`：同时提供原生工具和 Code Mode。

Code Mode 会把每个可见工具的精确参数和输出类型渲染为完整 SDK，并放入 System Prompt。它减少了
模型原生 Tool Schema 数量，也允许在代码中组合、并行和裁剪中间结果，但工具很多时 SDK 本身仍会
膨胀。其文档也明确不承诺 Code Mode 普遍降低 Token 成本。

### 3.5 调研结论

不存在所有 Agent 共同采用的单一 MCP 披露策略：

| 策略 | 优点 | 主要代价 |
| --- | --- | --- |
| 全量原生工具 | 模型原生参数约束强、调用直接 | Schema 成本随目录线性增长 |
| 搜索/描述/调用桥接 | Provider-neutral、顶层 ToolSet 稳定 | 一次任务可能增加搜索和加载 Step |
| 消息级动态 Schema | 加载后保留原生工具体验 | 依赖非通用 Provider 扩展 |
| Code Mode | 可在一次程序内组合多个工具并裁剪结果 | SDK/Catalog 仍可能膨胀，实现和安全边界复杂 |

## 四、ez-assistant 讨论收口

### 4.1 采用固定 MCP 网关工具

讨论确认：ez-assistant 不把所有 MCP 工具注册为模型原生工具，也不把完整 MCP Catalog 写入冻结
`SystemPromptSnapshot`。模型始终只看到一组稳定的 MCP 网关工具，具体能力像 Skill 一样按需披露
到 Conversation：

```text
search_mcp_tools
load_mcp_tools
call_mcp_tool
```

候选输入契约：

```text
search_mcp_tools {
  query,
  server?,
  cursor?,
  limit?
}

load_mcp_tools {
  names: [qualified_tool_name, ...]
}

call_mcp_tool {
  name: qualified_tool_name,
  arguments: object
}
```

固定三个工具比把 search/load/call 合成一个带复杂 `oneOf` 的工具更容易被不同 Provider 正确编码，
也便于分别定义无副作用、上下文变更和真实外部副作用。

### 4.2 渐进式发现

初始上下文不包含完整工具名称和 Schema。模型通过 `search_mcp_tools` 查询当前 Run Catalog：

```json
{
  "matches": [
    {
      "name": "mcp__github__create_issue",
      "server": "github",
      "description": "Create an issue in a GitHub repository"
    }
  ],
  "next_cursor": null
}
```

搜索结果保持紧凑，只提供选择工具所需的稳定名称、Server 和短描述。极大 Catalog 可以只在产品
诊断中显示 Server 状态和数量，模型侧无需接收完整 Server 清单。

搜索不产生外部副作用，使用普通 Tool Result 返回结果，不触发执行段 continuation。

### 4.3 `load_mcp_tools` 的 Step 后注入

`load_mcp_tools` 不把完整定义直接放进 Tool Result，只返回简短状态：

```json
{
  "ok": true,
  "status": "staged",
  "loaded": ["mcp__github__create_issue"],
  "catalog_revision": 12,
  "message": "The definitions become available after this tool batch finishes."
}
```

包含 `load_mcp_tools` 的 Tool Batch 按模型声明顺序正常执行，不跳过它后面的普通工具。完整批次结算
并持久化后，Runtime 才把成功加载的定义合并为一条 Runtime Hidden UserMessage：

```text
UserMessage
  origin = Runtime
  transcript_visibility = Hidden
  parts = [
    Injected(
      MCP_TOOL_DISCLOSURE_V1,
      catalog_revision,
      tools = [
        {
          qualified_name,
          server_id,
          raw_tool_name,
          description,
          input_schema,
          schema_digest
        }
      ]
    )
  ]
```

Provider 投影可以使用确定性文本包装，例如：

```xml
<mcp-tool-disclosure version="1" catalog-revision="12">
  <tool name="mcp__github__create_issue" server="github" schema-digest="...">
    ... escaped description and JSON schema ...
  </tool>
</mcp-tool-disclosure>
```

完整定义是规范 Conversation 的持久事实，而不是仅存在于某次模型请求或 Runtime 内存中的临时
状态。恢复时从消息历史重建已披露工具集合；不维护容易漂移的第二份 Session 权威账本。

### 4.4 同一个 Runtime Run 自动续段

MCP 定义加载复用 Skills 已讨论的执行段边界：

```text
Runtime Run r_123: Running
│
├── AgentExecution segment 0
│   ├── Model Step 调用 load_mcp_tools
│   ├── 按序完成整个 Tool Batch
│   ├── 原子持久化全部 Tool Call/Result
│   └── 结束为通用 ContinuationRequired
│
├── 持久化 MCP_TOOL_DISCLOSURE_V1 Hidden UserMessage
│
├── AgentExecution segment 1
│   └── 使用包含完整 MCP 定义的新 ConversationSnapshot 自动继续
│
└── 最终结算同一个 r_123
```

约束：

- 不创建第二个 Runtime Run；
- UI 不显示新的用户气泡或明显的执行截断；
- Step 编号、usage、预算、取消和事件序号在多个 AgentExecution segment 之间连续累计；
- Core 的 `ToolSetSnapshot` 始终不变，只包含固定 MCP 网关工具；
- 新定义通过 Conversation 进入下一 AgentExecution，不在原 AgentExecution 内修改 Tool Registry。

这与当前 Agent 架构中“单个 AgentExecution 使用不可变 ToolSetSnapshot”的约束一致。

### 4.5 同批调用语义

MCP 定义只在当前 Tool Batch 完整结算并完成披露消息持久化后生效。因此模型不应在同一个 Assistant
response 中同时加载并调用一个此前未披露的 MCP 工具。

如果同批出现：

```text
load_mcp_tools([X])
call_mcp_tool(name = X, ...)
```

两项仍按顺序结算，但后者应返回稳定错误：

```text
Tool X is available but not yet disclosed. The requested definition becomes active in the next step.
```

`load_mcp_tools` 的工具说明必须明确提示模型：依赖新 Schema 的调用应放在下一 Step。

### 4.6 固定 `call_mcp_tool` 的调用链

工具加载后，模型仍不直接调用 `mcp__github__create_issue` 这个 Provider 原生工具，而是始终调用：

```json
{
  "name": "mcp__github__create_issue",
  "arguments": {
    "owner": "example",
    "repo": "demo",
    "title": "Bug report"
  }
}
```

Runtime 执行顺序：

```text
确认工具属于当前 Run Catalog binding
→ 确认完整定义已经披露
→ 使用披露时冻结的 Schema 校验 arguments
→ 解析实际 MCP Server、raw tool name 和语义事实
→ 按实际 MCP 工具完成权限、审批、Guardrail 和并发分类
→ 调用 MCP tools/call
→ 转换并持久化普通 Tool Result
```

实际 `tools/call` 不触发特殊 continuation；结果按普通 Agent Tool Loop 回喂模型。

### 4.7 授权、审批与审计必须解包真实身份

不能只对固定网关名 `call_mcp_tool` 做授权，否则允许一次网关等价于允许所有 MCP 能力。

模型协议名称和安全主体必须分开：

```text
模型工具名：call_mcp_tool
安全与审计主体：mcp__github__create_issue
Server：github
raw tool name：create_issue
实际参数：已通过目标 Schema 校验的 arguments
```

Authorizer、Approval UI、Guardrail、调用指纹、审计日志和工具结果展示都应使用解析后的真实 MCP
身份与语义参数。Conversation 中仍使用 Provider 要求的固定 `call_mcp_tool` Call ID/Result 配对，
但持久元数据和产品投影应显示实际 MCP 工具。

### 4.8 命名与稳定身份

模型可见名称倾向采用：

```text
mcp__<server>__<tool>
```

但 Runtime 不得只保存规范化后的字符串，还应保存：

- 稳定 Server ID；
- Server 配置键；
- MCP 原始工具名；
- 模型可见 qualified name；
- Catalog revision；
- Schema digest。

若字符规范化造成碰撞，不得按注册顺序静默覆盖。正式设计需要确定拒绝冲突还是追加确定性短摘要；
无论选择哪种策略，诊断都必须显示冲突候选和原因。

### 4.9 与 Skills 的共同点和差异

共同点：

- 初始只披露可发现能力，不加载完整正文；
- 模型通过固定工具显式加载；
- 加载 Tool Result 只表示 staged 状态；
- 完整内容在 Tool Batch 后作为 Runtime Hidden UserMessage 持久化；
- 当前 AgentExecution 结束，同一 RunId 自动 continuation；
- UI 不展示人为的新 Run 或用户气泡。

差异：

- Skill 加载的是行为指令与工作流正文；
- MCP 加载的是外部能力说明和参数 Schema；
- Skill 后续使用普通工具完成工作；
- MCP 后续始终经固定 `call_mcp_tool` 调用；
- MCP 调用必须额外执行目标 Schema 校验、实际身份授权和远端连接状态检查。

因此两者可以复用通用的“Tool Batch 后持久化上下文并自动 continuation”能力，但不能合并为同一个
领域模型、Catalog、消息格式或调用工具。

## 五、安全与可靠性约束

1. MCP Server 的 `description`、`instructions`、Schema 注解和工具结果均视为外部不可信内容，
   不能直接提升为冻结 System Prompt。
2. 披露消息使用 Runtime 生成的确定性外壳，外部字符串和 JSON 必须严格编码，避免闭合标签或伪造
   Runtime 指令。
3. `load_mcp_tools` 不授予调用权限；实际 `call_mcp_tool` 每次都必须重新授权。
4. 只有当前 Run Catalog binding 中存在且已经披露的工具可以调用，不能靠猜测名称绕过加载。
5. 参数必须按披露时绑定的精确 Schema 校验；校验失败只产生错误 Tool Result，不发起 MCP 调用。
6. Tool Call/Result、披露 signal、Hidden UserMessage 和 continuation 必须具备可靠落账语义，不能
   出现 Tool Result 表示 loaded、正文却未持久化的半状态。
7. Server 断开、工具删除、Schema 更新和调用超时必须使用不同稳定错误，不把暂不可用伪装成未知。
8. 搜索结果数、单次加载数量、单个 Schema 大小、单 Run 加载次数和累计披露 Token 必须有显式上限。
9. Context Compaction 不应把外部 MCP 描述总结成高优先级长期指令。倾向丢弃已披露 Schema 并让模型
   按需重新加载，具体恢复语义留待正式设计。
10. MCP Tool Result 中的文本、图片、音频、资源链接和嵌入资源应先转为 provider-neutral 内容；不因
    MCP 来源绕过既有输出上限、附件持久化和敏感信息策略。

## 六、建议的模块边界

以下方向尚未成为正式技术方案：

- `assistant-runtime`：持有 MCP Server 配置、连接监督、Live Catalog、Run Catalog binding、披露
  状态、同 Run continuation、持久化和产品投影；
- Runtime Host / MCP Adapter：实现 stdio、Streamable HTTP、认证和 MCP SDK 接入，管理需要启动的
  Server 子进程或远程连接；
- `agent-core`：只看到固定 MCP 网关工具、普通 Tool Result 和通用 continuation 终态，不依赖 MCP
  Client、Session 或 RunId；
- `agent-tools`：承载固定网关 Tool Definition 以及 resolve/execute 所需的实现无关端口；是否引入
  专属公共 MCP trait，应以 Runtime 与第二个稳定宿主的真实调用边界为准；
- `assistant-protocol`：只公开 Desktop 需要的 Server 状态、Catalog 诊断、审批事实和调用展示，不
  泄漏 MCP SDK、Transport 或内部连接对象；
- Desktop：配置和展示 MCP Server、连接状态、诊断、审批和实际工具调用，不持有 Catalog 或连接
  权威状态。

MCP 工具按能力需要启动子进程不改变产品双进程拓扑；Session 不拥有独立 Runtime 子进程。

## 七、待正式设计确认

1. MCP 首版是否仅支持 Tools，暂不支持 Resources、Prompts、Sampling、Roots 和 Elicitation。
2. 首版 Transport 范围：stdio、Streamable HTTP 是否同时支持，以及旧 SSE 兼容边界。
3. Server 配置层级、启停、认证、秘密存储、连接重试和健康状态模型。
4. Live Catalog revision 的生成方式，以及 `tools/list_changed` 对活动 Run 和后续 Run 的精确影响。
5. 是否需要用户显式 MCP reload 指令，还是只依赖连接生命周期和协议通知。
6. `search_mcp_tools` 的检索方式、分页、排序、Server 过滤和目录极大时的 Token 预算。
7. `load_mcp_tools` 单批数量、Schema 大小、累计披露预算和加载循环上限。
8. `MCP_TOOL_DISCLOSURE_V1` 的规范消息类型、Provider 投影格式和压缩行为。
9. 通用 continuation signal、原子落账和重启恢复如何复用 Skills 与现有 Compaction 机制。
10. 固定 `call_mcp_tool` 如何在现有 resolved invocation 中投影实际工具身份、审批事实和并发分类。
11. MCP 名称规范化、碰撞处理、Server 重命名和持久稳定标识。
12. Server 在当前 Run 内断开或 Schema 更新时，已披露定义的失效、重试和错误语义。
13. MCP 图片、音频、资源链接、嵌入资源和大结果如何进入现有附件、裁剪与持久化链路。
14. 用户在 Desktop 中查看 Server、Catalog、已披露工具和 shadow/conflict 诊断的交互范围。

## 八、明确暂不确认

- 不因本次讨论直接启动新版本或修改当前版本状态；
- 不把 MCP 与 Skills 合并为同一包格式、Catalog 或领域对象；
- 不把完整 MCP Catalog 固定写入 `SystemPromptSnapshot`；
- 不要求每个 MCP 工具成为 Provider 原生 Tool Definition；
- 不依赖 Kimi 专属的消息级动态 Tool Schema 扩展；
- 不以 Code Mode 作为首版 MCP 的必要前提；
- 不让 MCP 描述、Server instructions 或 Tool Result 自动获得 System 权限；
- 不提前建立插件市场、远程 MCP Registry 或跨设备同步机制。
