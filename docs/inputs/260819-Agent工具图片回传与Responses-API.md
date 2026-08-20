# Agent 工具图片回传与 Responses API

- 记录日期：2026-08-19
- 状态：已吸收（保留历史输入）
- 来源：用户会话确认；OpenCode、Hermes Agent 与 OpenAI Responses API 实现调研
- 目标版本：`v0.17.0`（已完成）

> 范围吸收：最终设计结论见 [`v0.17.0 功能设计`](../versions/v0.17.0/功能设计.md)。
> 本版本已收窄为以 `read_image` 验证工具图片回流，不集成浏览器、CDP、Playwright
> 或 computer-use 具体操作工具；工具图片、三条识图路径、Responses 首版、本地权威状态、
> ProviderState 压缩和 Provider 启用边界均已确认。下文保留原始调研过程，其中的开放问题
> 和候选方案若与功能设计不同，以功能设计为准。

## 原始内容

在 `v0.16.0` 已完成用户附件图片理解的基础上，下个版本继续支持 Agent 执行过程中产生的图片，
例如浏览器或 computer-use 截图、无头浏览器页面渲染结果以及其他工具生成的图片。图片需要能够作为
可靠工具产物回传到后续模型调用：原生视觉主模型直接读取图片，文本主模型继续通过辅助识图模型取得
文本描述。

下个版本同时把 OpenAI Responses API 作为独立模型协议接入。该协议用于原生表达多模态
`function_call_output`、Responses item、reasoning 连续状态和后续 computer-use 能力；不替换现有
OpenAI-compatible Chat Completions，也不要求其他 Provider 统一迁移到 Responses API。

上述内容不进入 `v0.16.0`，避免扩大当前图片附件、reasoning、队列修复和 macOS 签名版本的复杂度。

## 原始候选边界（已由功能设计收口）

### 1. 工具图片产物是协议无关能力

- 工具生成的截图或图片先形成 Provider-neutral 的可靠工具产物，不能只返回临时本地路径或把 Base64
  正文写入 Conversation、SQLite、Journal 或普通调试事件。
- 图片继续复用统一文件资源语义，保存稳定文件引用、实际 MIME 和必要的产物关联；宽高等仅在预处理
  时实时提取，不向附件表混入图片专属业务字段。
- 原图是权威正文，缩略图只用于 Desktop 展示；模型调用继续复用公共图片预处理器，不建立按模型分叉
  的截图处理方案。
- 取消、超时、有限重试、权限、审计、历史恢复、上下文裁剪和附件生命周期必须覆盖工具生成的图片，
  不能只在当轮内存中临时拼接。

### 2. 后续模型调用的三种投影

- 当前主模型原生支持图片，且协议允许多模态 Tool Result：将工具文本和图片产物直接投影给主模型。
- 当前主模型原生支持图片，但协议或具体服务不接受图片形式的 Tool Result：保留可靠文本 Tool Result，
  再生成一条内部图片输入供下一轮模型读取；该内部投影不得伪装成用户新提交的业务消息。
- 当前主模型不支持图片：把图片交给已配置的默认辅助视觉模型，主模型只接收识别文本和必要的稳定
  产物引用。

能力判定继续来自 `(provider, protocol, model_id)` 静态目录和用户 override，不能仅根据模型名称猜测，
也不能在请求失败后静默切换协议。

### 3. Responses API 接入边界

- 在模型协议枚举中新增明确的 `openai_responses`，由独立 Adapter 负责编解码；
  `openai_chat_completions` 继续保留。
- Responses Adapter 将规范多模态工具产物编码为 `function_call_output.output` 中的有序文本与
  `input_image` Part，并完整处理流式 message、function call、reasoning、usage、取消、错误和未完成状态。
- Runtime 继续持有 Session、Conversation、Run 和持久化的权威状态。首版优先采用本地规范历史回放，
  不把 `previous_response_id` 或远端 Conversation 当作唯一会话事实。
- 如果 reasoning 连续性要求回传 Responses item 或加密状态，只保存协议所需的受控不透明状态，并按
  Provider/Endpoint 隔离；模型切换时不能把一个服务签发的状态发送给另一个服务。
- 静态模型目录只为官方或实测确认支持 Responses 的 `(provider, protocol, model_id)` 组合登记能力；
  Chat-only 代理和通用 OpenAI-compatible Endpoint 不自动升级。

## 原始核查清单（已完成收口）

1. `ToolResultContent` 如何表达有序文本、JSON 与文件产物，同时保持旧持久化数据兼容。
2. 浏览器截图、computer-use 截图和普通工具文件输出是否共用一个 Artifact/File Reference 契约。
3. Chat Completions 的内部图片输入应在规范 Conversation 中保存，还是在模型视图阶段确定性重建。
4. 图片工具产物随上下文压缩、Fork、Session 导出导入和文件清理时的保留规则。
5. Responses 的流式事件、reasoning item、`store: false`、远端状态复用和 prompt cache 之间的取舍。
6. 首批启用 `openai_responses` 的 Provider、模型和真实连接验证矩阵。

## 调研依据

- [OpenCode Responses 协议实现](https://github.com/anomalyco/opencode/blob/dev/packages/llm/src/protocols/openai-responses.ts)
- [OpenCode Chat Completions 多模态工具结果兼容](https://github.com/anomalyco/opencode/blob/dev/packages/llm/src/protocols/openai-chat.ts)
- [OpenCode 会话媒体 Tool Result 路由](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/message-v2.ts)
- [Hermes browser vision](https://github.com/NousResearch/hermes-agent/blob/main/tools/browser_tool.py)
- [Hermes 原生视觉与辅助视觉路由](https://github.com/NousResearch/hermes-agent/blob/main/tools/vision_tools.py)
- [Hermes Responses Adapter](https://github.com/NousResearch/hermes-agent/blob/main/agent/codex_responses_adapter.py)
- [OpenAI Computer use](https://developers.openai.com/api/docs/guides/tools-computer-use)
- [OpenAI Conversation state](https://developers.openai.com/api/docs/guides/conversation-state)
