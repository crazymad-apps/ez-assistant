# v0.16.0 技术方案附件 A：多模态消息、识图工具与模型协议

## 一、附件信息

- 状态：已确认（2026-08-18）
- 主方案：[`v0.16.0 技术方案`](技术方案.md)
- 功能基线：[`v0.16.0 功能设计`](功能设计.md)
- 关联附件：[`附件 B：会话 effort 与桌面交互`](技术方案附件B-会话effort与桌面交互.md)

本文落实统一文件引用、公共图片预处理、受控资源解析、原生多模态编码、辅助识图工具以及
OpenAI/GLM/Qwen/Kimi/DeepSeek 模型协议适配。Provider 结论以 2026-08-18 官方文档为准。

## 二、统一文件引用与公共图片预处理

### 2.1 `agent-types` 数据结构

保留现有 `UserPart::FileReferences` 和 `FileReference`，不增加图片专属字段：

```rust
pub struct FileReference {
    pub original_name: String,
    pub readable_path: String,
}
```

命名和字段可在实现时按现有风格微调，但以下约束不可改变：

- 图片和普通文件保存在同一个 `FileReferencesPart.files` 中，完整保留用户选择顺序并作为同一用户
  消息参与 Fork、重放和上下文裁剪。
- `readable_path` 是 Runtime 分配、工具与模型资源解析器都能识别的稳定引用，不是 WebView 传入的
  任意本地路径。
- 不引入应用层 `AttachmentId`，避免 `agent-types -> assistant-protocol` 反向依赖。
- Conversation 不保存 MIME、宽高、Base64、Data URL、Provider file ID、`detail`、像素上下限或
  转换后的临时路径。实际 MIME 作为 Blob 文件事实单独保存在 Attachment Registry。
- 图片身份以 Host 检测并保存的实际 MIME 为候选依据，公共预处理时仍校验实际字节；Runtime、
  Desktop、工具和 Adapter 都不得根据扩展名保存或推导第二份可信分类事实。

### 2.2 从附件到 User Part

`submit_input` 继续接收附件 ID。Runtime 在幂等落库前从附件 Registry 解析 ready 附件：

1. 按附件 ID 的原始顺序生成一个 `FileReferencesPart`，图片和普通文件使用完全相同的引用结构。
2. 文本非空时仍先形成 Text Part；不按媒体类型拆分文件列表，因此图片和普通文件的交错顺序不会丢失。
3. 规范消息和输入事实仍在现有持久化事务/补偿边界内提交；任何引用解析失败都不产生半条消息。
4. 不改写历史 Conversation；历史文件引用在首次缩略图请求或模型资源解析时按实际内容识别。

### 2.3 公共预处理策略

原生多模态和辅助识图工具共用 Host 中唯一的 `ModelImagePreprocessor`。它接受静态 PNG、JPEG、
WebP 和非动画 GIF 原图，并在每次图片实际进入 LLM 请求前完成以下处理：

1. 通过受控文件引用读取原始 Blob，按文件签名嗅探并解码，不信任扩展名或浏览器声明；
2. 在内存中读取原始宽高、方向和像素总量，执行解码安全限制；宽高不写 SQLite 或 Conversation；
3. 应用 EXIF 方向，转换到 sRGB，移除元数据；透明区域使用固定浅色背景合成；
4. 保持宽高比、不放大小图，按一套应用级最大边和总像素限制缩小；
5. 统一编码为各目标模型共同接受的 JPEG，并满足一套应用级单图输出字节上限；
6. 动画 GIF、损坏图片、极端宽高比或无法满足统一输出上限的图片返回附件级错误，不按 Provider
   临时采用另一条处理路径。

M0 使用同一组真实图片 fixture 对 OpenAI、GLM、Qwen、Kimi 的目标模型验证后，锁定唯一的应用级
最大边、总像素、输出字节和单次请求图片数量。当前候选基线为最大边 `2048 px`、单图 JPEG 不超过
`5 MiB`；这是全局实现常量，不是用户配置或模型 capability。若验证后需要调整，只调整这一套公共
策略，不在协议适配层增加缩放、压缩、转码或画质分支。

预处理结果只活到本次模型调用结束；同一次调用的有限重试复用同一份结果，避免重复解码。原始 Blob
始终是附件权威正文，不保存 LLM 预处理副本。图片格式不属于上述集合时仍可作为普通附件使用，但
公共预处理器返回“不是受支持图片”，Adapter 不得自行猜测或补救。

### 2.4 面向模型的能力投影

规范 Conversation 始终只保存统一文件引用，模型视图按能力逐项编码：

- 原生图片模型：模型请求准备阶段按文件列表顺序调用公共预处理器；成功识别的图片形成
  `PreparedModelImage`，非图片仍形成稳定文本引用，Codec 只编码准备后的有序视图；图片
  `readable_path` 不作为普通文本泄露给 Provider；
- 文本主模型且已注册 `inspect_images`：模型视图把图片候选渲染成有界文本清单，明确列出可传给
  `inspect_images.image_paths` 的稳定路径和文件名，不附带图片字节；工具执行时再由同一公共预处理器
  验证实际内容并生成图片输入，普通文件继续使用既有引用；
- 当前不可识图：仍渲染“存在未读取的图片附件”提示，禁止模型声称已经看过图片，也不向其暴露一个
  实际不可调用的工具指令。

模型请求准备阶段遍历同一文件列表，必要时把连续普通文件合并为文本 Part，但不得改变图片与普通文件
的相对顺序。该投影不向规范 Conversation 注入重复提示。工具解析器再次校验 path 是否属于当前
Session；模型仅知道字符串并不构成访问授权。

### 2.5 历史消息中的图片回放

当前共同基线是无状态 Chat Completions。只要 Context Layout 仍选择某条包含图片文件引用的历史用户
消息，就按本次 Run 的模型能力重新投影：

- 当前模型原生支持图片：从稳定文件引用重新读取原始 Blob，执行同一公共预处理，并在该历史用户消息
  的对应位置重新生成 `image_url` Part；不能只回放文件名或过去的 Assistant 文本回答；
- 当前模型通过 `inspect_images` 识图：不向文本主模型发送 `image_url`，保留稳定图片路径、历史 Tool
  Call/Result 和已有识别文本；模型确有需要时可再次调用工具；
- 当前模型不可识图：只投影未读取图片提示，不伪造图片内容。

这里的“回放图片”是根据规范文件引用重新生成当前请求的 Data URL，不是保存或原样复用历史 Provider
请求中的 `image_url`、Base64、公网 URL 或 file ID。切换模型或 Provider 后按当前能力和当前协议重新
投影，不改写 Conversation。Context Layout 裁剪历史时，图片文件引用必须与所属用户消息整体保留或
整体裁剪；不能保留文字却单独静默删除该消息中的图片。

若被选入上下文的历史图片已经不可用、内容与已存实际 MIME 不一致或公共预处理失败，必须在 Provider
建流前返回可诊断资源错误，不能省略图片后继续请求。重新生成的图片输入会再次占用请求体和视觉
token；本版本不通过持久化 Provider file ID 或历史 Data URL 优化该成本。

## 三、Blob 派生资源与模型资源解析

### 3.1 原图与缩略图文件

`attachment_blobs` 增加一个适用于所有文件的可空实际 MIME 字段，不在 `attachments` 增加图片业务
字段：

```text
media_type  TEXT NULL
```

新上传 Blob 在稳定提交前按文件签名检测实际 MIME 并写入；不信任扩展名或客户端声明。旧 Blob 保持
`NULL`，不批量扫描或回填；首次模型资源解析或缩略图请求时实时嗅探，但本版本不要求把结果回写。
图片宽高、像素总量、图片类型和缩略图路径均不入库。

原始 Blob 仍使用现有内容哈希和原始扩展名命名；缩略图是与原图相邻的派生文件：

```text
blobs/sha256/ab/<blob_hash>.png
blobs/sha256/ab/<blob_hash>.png.thumbnail.jpg
```

没有原始扩展名时分别使用 `<blob_hash>` 和 `<blob_hash>.thumbnail.jpg`。缩略图固定输出 JPEG、最大边
`320 px`，通过临时文件加原子改名提交；它不参与 Blob 哈希、不创建第二条 Attachment，也不进入
SQLite。原图去重时复用同一缩略图；原图被清理时同时清理派生缩略图。派生文件缺失或损坏时允许从
原图重新生成，不能影响原始附件的可读性。

### 3.2 缩略图生成

新附件的原始 Blob 稳定提交后，Host 使用与 LLM 公共预处理器相同的嗅探、解码、方向和安全校验
组件生成缩略图，但使用独立且固定的 `320 px` UI 输出尺寸。缩略图不能作为 LLM 输入，模型调用始终
从原始 Blob 实时预处理。

历史 Blob 不批量扫描或改写。Desktop 对 `media_type` 为 `image/*` 的 ready 附件请求缩略图；旧附件
的 `media_type` 为空时仍可尝试缩略图资源。派生文件不存在时，Host 按需嗅探和解码实际内容，图片则
生成并保存，非图片返回不支持的媒体错误。Desktop 收到该错误后回退到普通附件样式，不在前端保存
基于扩展名的图片分类。

图片解码依赖优先只放在 `apps/runtime-host`，使用 `image` crate 并关闭默认特性，只启用批准的
PNG/JPEG/WebP/GIF decoder。版本和许可证由 M0 锁定进 `Cargo.lock`。

### 3.3 公共模型图片预处理契约

Provider Adapter 不允许使用 `std::fs`/`tokio::fs` 或理解 Runtime Home。`agent-model` 增加窄的
异步预处理能力，由 Host 实现，并由原生图片调用和 `AuxiliaryVisionInspector` 共同使用：

```rust
pub trait ModelImagePreprocessor: Send + Sync {
    fn prepare<'a>(
        &'a self,
        reference: &'a FileReference,
        cancellation: &'a CancellationToken,
    ) -> ModelImagePreparationFuture<'a>;
}

pub enum ModelImagePreparation {
    Image(PreparedModelImage),
    NotImage,
}

pub struct PreparedModelImage {
    pub media_type: String,
    pub bytes: Arc<[u8]>,
}
```

实现必须通过 Attachment Registry/受控路径解析，拒绝任意绝对路径、越界资源和非 ready 附件。已存
`media_type` 用于跳过明确的非图片文件，但图片仍须校验实际字节与记录一致；旧记录为空时实时嗅探。
实际内容不是图片时返回 `NotImage`，由模型视图继续作为普通文件引用；具有图片签名但损坏、动画或
不能满足公共限制时返回明确错误。`Image` 结果已经符合唯一公共策略，首版输出 `media_type` 固定为
`image/jpeg`。Codec 是纯编码器：接收规范消息和已预处理资源，生成请求 JSON；它不执行 I/O、解码、
缩放或转码。

原生多模态调用在建流前预处理全部本次需要的图片。任一图片失败即整次模型调用失败，错误包含可诊断
图片索引和原因但不包含字节。取消发生时停止尚未完成的解析和请求构造。

## 四、模型能力与上下文

`ModelCapabilities` 扩充但继续保持 Provider-neutral：

```rust
pub struct ModelCapabilities {
    pub reasoning: bool,
    // 由目录 effort_map 的 key 编译，不是第二份配置来源。
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub default_active_reasoning_effort: Option<ReasoningEffort>,
    pub image_input: bool,
    pub tool_calls: bool,
    pub streaming: bool,
}
```

本版本不增加统一的图片 token 预留字段。视觉 token 取决于 Provider、具体模型、分辨率、缩放规则及
OpenAI `detail` 等协议参数，固定的“每张图片 token 数”没有可验证算法，不能可靠用于上下文预检。
Context Layout 仍将统一文件引用 Part 与所属用户消息作为整体保留或裁剪；发送前只执行已明确的图片
格式、尺寸、数量和请求体限制，Provider 返回的实际 usage 作为计费与观察权威。若 Provider 报告
context overflow，沿用现有受限压缩/重试机制，不增加无限重试。未来只有在具体 Provider/模型提供
确定的本地计算公式或 token 计数接口，并完成 fixture 验证后，才增加协议侧 estimator，不在
`ModelCapabilities` 中保存人工配置的近似常量。

### 4.1 术语与静态模型目录

本项目不再把服务路由和协议兼容层都称为 Provider：

| 术语 | 唯一含义 |
| --- | --- |
| `model_key` | 用户创建的一条模型配置实例，也是 Session 的稳定引用；同一 Provider、模型或 Endpoint 可以存在多条配置 |
| `provider` | 实际接收请求的服务身份，例如 OpenAI、DashScope、Moonshot、OpenRouter 或用户自建服务；不表示协议实现，也不等同于 `model_key` |
| `model_id` | 发送给该服务的上游模型标识；模型品牌/作者只作为展示信息，不参与路由 |
| `protocol` | 配置与静态目录使用的协议族，例如当前的 `openai_chat_completions`，未来可增加 `openai_responses`、`anthropic_messages` |
| `capabilities` | `(provider, protocol, model_id)` 编译后的图片、工具、reasoning 和 effort 能力事实 |

其中，Endpoint 和 credential 属于 `model_key` 指向的 Route 配置；`provider` 只是该 Route 中稳定、可查表
的服务身份。同一 Provider 下的不同账号、区域 Endpoint 或代理连接不因此产生新的 Provider 概念。
用户自建或无法确认实际服务身份的连接可以使用 `custom`/`local` 等自定义 Provider ID，并按未收录模型
处理。

对外部项目的术语核对如下：

| 项目 | 服务/模型标识 | 协议兼容层 | 本项目取舍 |
| --- | --- | --- | --- |
| OpenCode | 以 `providerID + modelID` 选模型；Provider 配置承载连接信息 | 具体 SDK/provider package 与 options 处理协议差异 | 保留服务和模型分层，同时让 `protocol` 显式参与本项目查表，但不把实现包称为另一种 Provider |
| Hermes Agent | `provider` 表示推理服务或自定义连接 | 配置使用 `transport`，Provider registry 使用 `api_mode`/`ProviderProfile` 表达调用方式和能力 | 项目自身继续使用已有的 `protocol`，不引入 `api_mode` 或 `ProviderProfile` |

因此，协议兼容层不再使用任何 Provider 派生名称。现有 `ModelProfile`、
`ModelCompatibilityProfile` 仅是待迁移旧名，不再作为领域术语。界面显示服务商
名称时使用“服务商”，显示协议时使用“协议”；普通用户不需要理解内部协议适配器。

Runtime Host 随包提供只读程序资源 `apps/runtime-host/resources/model-catalog.json`。它不属于用户
`config.toml`，不提供设置页或手工编辑入口，只能随软件版本更新。目录使用项目已有的 `serde_json`
解析，不增加 YAML、JSON5 或 RON 依赖；JSON 不承载注释，核查来源和维护说明放在文档与 fixture 中。
目录只保存数据，不允许定义任意请求字段或脚本；示意：

```json
{
  "schema_version": 1,
  "catalog_revision": "2026-08-19",
  "models": [
    {
      "provider": "moonshot",
      "provider_label": "Moonshot（Kimi）",
      "protocol": "openai_chat_completions",
      "protocol_label": "Chat Completions（OpenAI Compatible）",
      "model_ids": ["kimi-k3"],
      "capabilities": {
        "image_input": true,
        "reasoning": {
          "enabled": true,
          "default_effort": "max",
          "effort_map": {
            "low": { "label": "Low", "wire_value": "low" },
            "high": { "label": "High", "wire_value": "high" },
            "max": { "label": "Max", "wire_value": "max" }
          }
        }
      }
    }
  ]
}
```

Kimi Code 使用独立 Endpoint 与模型 ID `k3`，因此目录中另设一条精确 Route，并将其默认 effort
设为 Kimi Code 文档声明的 `high`；不能与公共 API 的 `kimi-k3`（默认 `max`）合并为同一能力条目。

`effort_map` 的 key 只能使用产品稳定档位 `low`、`medium`、`high`、`xhigh`、`max`；这些 key 同时
构成该模型实际支持的 effort 列表，不再另存 `reasoning_efforts`。`label` 是 Desktop 原样展示的文本；
`wire_value` 是字符串或正整数，只能作为值交给由 `provider + protocol` 选中的 Adapter，目录不得配置
原生字段名、JSON path 或任意请求片段。Adapter 必须校验 wire value 的类型、范围和目标字段兼容性后
才能形成有效配置。

`default_effort` 必须引用 `effort_map` 中存在的 key。支持 reasoning 但没有强度概念的模型只设置
`enabled = true`，省略 `default_effort` 和 `effort_map`；不支持 reasoning 时省略整个 reasoning 块或
显式设置 `enabled = false`。

目录以精确 `(provider, protocol, model_id)` 为键；同一服务和模型通过不同协议入口调用时必须分别
列项，因为它们的图片格式、reasoning、effort、工具调用和历史回放能力可能不同。一个条目可以枚举
已核实的精确模型别名，但不得使用可能误收未来模型的模糊正则或名称前缀。

`provider_label` 和 `protocol_label` 是随包目录的展示元数据。Runtime 在查询脱敏模型列表时同步投影
目录 revision 和精确路由；Desktop 由该投影生成 Protocol、Provider 和 Model ID 建议项，不在前端
硬编码服务商名单。Provider 和 Model ID 使用可自由输入的单选下拉，因此目录外的新型号仍可按
保守 OpenAI-compatible 基线配置；表单型下拉层左对齐触发输入项，最小宽度与输入项相同。

配置编译优先级固定为：

1. 从用户模型配置读取并校验 `provider`、`protocol` 和 `model_id`；当前只有一个可选协议时，界面可以
   默认选中 `openai_chat_completions`，但持久化配置和编译结果仍显式携带它；
2. 用户显式 capability override；其中 reasoning override 必须以完整块替换，不能把用户
   `default_effort`、静态目录 `effort_map` 和协议默认值逐字段拼接；
3. 随包静态目录中精确匹配的 `(provider, protocol, model_id)` 条目；
4. 目录未命中时保留已选择的 `protocol`，并使用该协议的保守能力基线：默认不声明图片输入和
   reasoning，effort 列表为空；如果 `protocol` 本身没有对应 Adapter 实现，则配置编译失败，不能静默
   改用 OpenAI 协议。用户确知自定义服务能力时可以显式覆盖 capability。

正常用户和模型设置页不需要填写 capability。`protocol` 是模型配置字段，当前只有一个支持项时可以由
界面默认填写；capability override 是自定义 Endpoint、新模型抢先接入和兼容排障的高级入口，不能拼接
wire 字段。protocol 与能力事实无法兼容时配置编译失败，不静默删除或改写。

高级 override 如需声明 reasoning effort，复用同一份受限 `effort_map` schema 并整体覆盖 reasoning
块；它仍只能提供 `label + wire_value`，不能指定 Provider 原生字段名。这样既保留用户配置优先级，也
不会把静态目录变成任意请求模板系统。

现有 `protocol = "chat_completions"` 在配置 schema 升级时迁移成更具体的
`protocol = "openai_chat_completions"`。新配置显式保存 `protocol`，由它确定基础 API 族并作为静态目录
匹配键的一部分；具体服务差异由 `provider + protocol` 共同选择协议适配，不再引入第二个协议字段。

目录中的 `default_effort` 编译为 `default_active_reasoning_effort`，表达“Session 选择默认时，为保证
thinking 开启而实际发送的模型档位”。它是编译后的模型能力事实，不是用户偏好；没有 effort 概念、
但可用独立字段开启 thinking 的模型可以为空。

厂商在既有协议下新增或调整模型时，只修改目录和 fixture，不改 Rust 源码；新增请求协议、字段语义
或历史回放规则时仍须实现新的协议适配。目录作为 macOS 已签名 App 资源发布，更新后需要重新
打包、签名和公证；本版本不实现绕过 App 签名的远程热更新目录。

## 五、模型协议适配重构

### 5.1 内部结构

删除允许调用方用字段字面量任意拼接的扁平 `Profile`。协议 Adapter crate 对外只接收具名
`ModelProtocol`，当前 Chat Completions 实现内部使用字段私有的组合：

```rust
struct ChatCompletionsProtocol {
    request: RequestFormat,
    image: ImageFormat,
    reasoning: ReasoningFormat,
    tools: ToolFormat,
    usage: UsageFormat,
}

struct ReasoningFormat {
    enable_wire: ReasoningEnableWire,
    effort_wire: Option<ReasoningEffortWire>,
    response_field: Option<&'static str>,
    replay: ReasoningReplayPolicy,
}

enum ReasoningReplayPolicy {
    Drop,
    ToolCallsOnly,
    PreserveAll,
}
```

`ImageFormat` 只表达图片 Part 的 JSON 字段形态和 Data URL 放置位置，不包含分辨率、质量、压缩格式或
转码函数。所有协议适配接收相同的 `PreparedModelImage`；不允许在 `openai()`、`zhipu()`、
`dashscope()`、`moonshot()` 等构造中配置不同预处理策略。

具体名称可按 crate 风格调整。`ModelProtocol` 当前只有 `openai_chat_completions`；未来按真实需求增加
`openai_responses`、`anthropic_messages` 等协议族，不为 DeepSeek、GLM、Qwen 或 Kimi 创建伪协议名。
字段级 builder 只允许 crate 测试使用，不能让 Runtime 自行拼字段。现有
`ModelCompatibilityProfile` 合并到由 `provider + ModelProtocol` 选择的协议适配中；Host、各 Demo、
可靠性录制/回放和测试调用点全部迁移。路由同时携带配置中的实际 `provider` 和 `protocol`；静态目录
按 `provider + protocol + model_id` 解析能力，二者不再共用 Provider 一词。

现有 `crates/agent-provider-openai-compatible` 在本版本实现阶段重命名为
`crates/agent-openai-compatible`，Cargo package、workspace member、依赖声明、模块约束、测试和文档引用
同步迁移。该 crate 只负责 OpenAI-compatible 模型协议适配，不再以 Provider 命名；实际服务身份仍由
Runtime Route 中的 `provider` 表达。

### 5.2 图片共同基线

OpenAI、GLM、Qwen 和 Kimi 的官方 Chat 接口都能表达文本 Part 与图片 Part，但来源限制不同。本版本
从公共预处理结果生成 Data URL，采用对象形态作为共同规范输出：

```json
{
  "role": "user",
  "content": [
    { "type": "text", "text": "请识别图片" },
    {
      "type": "image_url",
      "image_url": { "url": "data:image/jpeg;base64,<prepared-bytes>" }
    }
  ]
}
```

Data URL 只存在于传输对象。Kimi 当前视觉指南不把公网 URL 作为通用途径，因此本版本不以公网 URL
作为共同基线；Qwen 像素扩展和 OpenAI `detail` 不进入规范消息、预处理策略或 UI。若某 Endpoint
不接受对象形态，只能由具名协议适配转换 JSON 结构；若它不接受公共 JPEG 预处理结果，则该模型配置判定
为图片输入不兼容，不为其增加专属图片转换链。

### 5.3 Thinking/reasoning 协议表

| 协议 | 默认开启编码 | effort | 响应 | 历史回放策略 |
| --- | --- | --- | --- | --- |
| OpenAI | 按目标模型的正式 Chat 字段；默认选择模型 capability 声明的 active effort，不发送 `none` | 支持模型使用 `reasoning_effort`；GPT-5.6 capability 默认 active key 为 `medium` | 协议 Adapter 解码 reasoning Part | 工具链按官方协议回放；普通历史遵循目标模型要求 |
| Zhipu/GLM | `thinking: {"type":"enabled"}` | GLM-5.2+ 接受原生档位，但只向产品公布有效不重复档位 | `reasoning_content` | 默认只保留工具调用链；显式全保留才发送 `clear_thinking:false` |
| DashScope/Qwen | `enable_thinking:true`；`qwen3.8-max` 同时显式发送 `preserve_thinking:true` | `qwen3.8-max` 的 Chat 接口直接使用 `reasoning_effort`，公布 `low/medium/xhigh`，默认 `xhigh` | `reasoning_content` | `qwen3.8-max` 使用 `PreserveAll`，后续请求完整回传历史 `reasoning_content` |
| Moonshot/Kimi | K3 是始终思考模型，不再发送 K2.x 的 `thinking` 开关 | 公共 API `kimi-k3` 为 `low/high/max`、默认 `max`；Kimi Code `k3` 同为三档、默认 `high` | `reasoning_content` | `kimi-k3` 与 `k3` 均完整回放历史 reasoning |
| DeepSeek | `thinking:{"type":"enabled"}` | V4 公布 `high/max`，默认 `high` | `reasoning_content` | 工具调用链必须回传原始 reasoning |
| Generic | 不发送私有开启字段 | 无显式档位 | 只解码标准已声明字段 | 默认 Drop；不猜私有回放要求 |

GLM-5.2 文档所示低/中档映射到同一高强度、高以上映射到最大强度时，UI 只显示有不同实际效果的
协议选项，例如“高”“最大”，稳定 key 分别选 `high`、`max`；不显示会产生同一 wire 行为的别名。

Qwen Responses API 的七级 `reasoning.effort` 不直接复制到当前 Chat Completions Adapter；当前只实现
`qwen3.8-max` Chat 文档明确公布的三档 `reasoning_effort`。未命中随包目录的旧型号或新型号均不
猜测该映射。Kimi effort 同样只为 K3 声明，不向其他批次外推。

### 5.4 reasoning 历史的编码规则

- `Drop`：普通历史和工具链均不编码 reasoning，只能用于不要求回传的协议。
- `ToolCallsOnly`：仅当规范 Assistant Message 同时包含 Tool Call 时回传原始 Reasoning Part；保持
  Tool Call/Result 链完整，普通已完成轮次不重复携带思考正文。
- `PreserveAll`：回传所有规范 Reasoning Part，并发送协议要求的显式保留开关。首版没有 UI 开关，
  只在具体 Provider/模型明确要求时由协议适配选择。

Codec 不生成“已省略思考”等占位内容。解码不到 reasoning 时正常缺省；能力不能由一次响应是否包含
`reasoning_content` 反推。

`default_active_reasoning_effort` 属于编译后的模型能力，不是 Session 配置项。配置编译时，目录
`default_effort` 必须属于 `effort_map`；不属于时该模型配置失败，不能在 Run 时临时猜测。没有 effort
概念但能显式开启 thinking 的模型保持 `None`，由协议适配中的 `enable_wire` 保证开启。

### 5.5 官方核查来源

随包目录于 2026-08-19 按各服务商当时最新批次初始化。首版只收录下表精确稳定 ID；旧批次、已弃用
别名、只有快照 ID 的型号以及尚未从官方文档确认的能力不进入目录。目录未命中时继续使用协议保守
基线，用户可通过高级 capability override 抢先接入。

| Provider | 首批模型 | 已核实映射 | 官方依据 |
| --- | --- | --- | --- |
| OpenAI | `gpt-5.6`、`gpt-5.6-sol`、`gpt-5.6-terra`、`gpt-5.6-luna` | 图片、流式、Function Calling；active effort 为 `low/medium/high/xhigh/max` | [GPT-5.6 模型目录](https://developers.openai.com/api/docs/models) |
| Zhipu | `glm-5.2` | thinking；有效 effort 收敛为 `high/max` | [核心参数](https://docs.bigmodel.cn/cn/guide/start/concept-param)、[深度思考](https://docs.bigmodel.cn/cn/guide/capabilities/thinking) |
| Zhipu | `glm-5v-turbo` | 图片、thinking；不把 GLM-5.2 effort 推断到视觉型号 | [模型概览](https://docs.bigmodel.cn/cn/guide/start/model-overview)、[GLM-5V-Turbo](https://docs.bigmodel.cn/cn/guide/models/vlm/glm-5v-turbo) |
| DashScope | `qwen3.8-max` | 图片、thinking；Chat effort 为 `low/medium/xhigh` | [视觉理解](https://help.aliyun.com/zh/model-studio/vision-model/)、[OpenAI-compatible Chat](https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-chat-completions) |
| Moonshot | `kimi-k3` | 图片、始终 thinking；effort 为 `low/high/max`，默认 `max` | [Kimi K3](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart)、[Reasoning Effort](https://platform.kimi.ai/docs/guide/use-reasoning-effort) |
| Moonshot（Kimi Code） | `k3` | 图片/视频输入、始终 thinking；effort 为 `low/high/max`，默认 `high` | [Kimi Code Models](https://www.kimi.com/code/docs/kimi-code/models.html) |
| DeepSeek | `deepseek-v4-flash`、`deepseek-v4-pro` | thinking；effort 为 `high/max`，默认 `high` | [模型与价格](https://api-docs.deepseek.com/quick_start/pricing/)、[Thinking Mode](https://api-docs.deepseek.com/guides/thinking_mode/) |

补充协议依据：

- [OpenAI Images and vision](https://developers.openai.com/api/docs/guides/images-vision)
- [GLM 思考模式与历史回放](https://docs.bigmodel.cn/cn/guide/capabilities/thinking)
- [Kimi Chat API](https://platform.kimi.ai/docs/api/chat)
- [Kimi Vision](https://platform.kimi.ai/docs/guide/use-kimi-vision-model)
- [DeepSeek Chat Completions](https://api-docs.deepseek.com/api/create-chat-completion/)
- [OpenCode Models：provider/model、catalog 与 capability](https://opencode.ai/v2/docs/models)
- [OpenCode Providers：provider package 与 OpenAI-compatible Adapter](https://opencode.ai/docs/providers)
- [Hermes Providers：provider 与 transport/api mode](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/integrations/providers.md)
- [Hermes ProviderProfile registry](https://github.com/NousResearch/hermes-agent/blob/main/providers/README.md)

## 六、辅助识图工具

### 6.1 工具契约

在 `agent-tools` 注册普通工具 `inspect_images`：

```json
{
  "type": "object",
  "properties": {
    "image_paths": {
      "type": "array",
      "items": { "type": "string" },
      "minItems": 1
    },
    "goal": { "type": "string", "minLength": 1 },
    "background": { "type": "string" }
  },
  "required": ["image_paths", "goal"],
  "additionalProperties": false
}
```

`goal` 同时表达识别目标与关注点，不再设置 `focus` 或 `expected_output`。`background` 可选，未知时
不要求主模型编造。`image_paths` 只能引用当前 Session 规范消息/附件 Registry 中允许的图片资源。

工具输出是 `ToolResultContent::Text`，不是强制结构化 JSON。辅助模型直接输出面向主模型的识别文本；
固定 system prompt 要求在相关时给出关键观察、OCR、不确定项并围绕 goal 作答。随后主模型在原有
Agent Loop 中读取工具结果并完成总结，因此工具内部不得再调用第二次“总结模型”。

### 6.2 能力壳和装配

`agent-tools` 只定义实现无关的 `ImageInspector` trait 和工具壳。Runtime Host 提供
`AuxiliaryVisionInspector`：

1. 从已编译配置中取得 `agent.vision.model_key` 对应 Model Service；
2. 验证该服务明确支持 `image_input`；
3. 用固定 system prompt、从统一文件引用筛选出的图片、goal 和可选 background 构造一次
   Provider-neutral 请求；
4. 强制 `ToolChoice::None`，不注册任何工具，不启动嵌套 Agent；
5. 收集一次文本终态并返回普通 Tool Result。

Runtime 的 Run Tool Factory 仅在主模型不支持图片、主模型支持 Tool Call、辅助视觉模型有效时注册
该工具。主模型原生支持图片时不额外注册同名识图工具，避免模型在两条路径间随机选择。

辅助视觉配置示意：

```toml
[agent.vision]
model_key = "vision"
timeout_ms = 60000
max_output_tokens = 4096
```

credential 仍只存在于被引用的模型配置。Session effort 不传给辅助视觉模型；辅助模型使用其协议
默认 thinking/effort，避免把主模型相对强度错误套到另一模型。

### 6.3 取消、重试和 usage

- 工具调用继承父 Run 的 cancellation token；资源读取、建流和收流都必须响应取消。
- 有效超时是视觉工具配置超时与父工具剩余预算的较小值。
- 建流前瞬态错误可复用现有有限 `ModelRetryPolicy`；已产生可见输出或取消后不重试。
- `ToolResult` 增加不进入模型正文的观察 metadata，至少记录辅助模型 identity、token usage、耗时和
  attempt 数；Adapter 只编码文本 content。
- Desktop 的既有工具卡片在详情中显示“辅助视觉模型”和独立 usage。Session 主模型 usage 不吞并
  辅助 usage；聚合报表可同时展示 main/auxiliary 两栏。

## 七、桌面图片投影

`AttachmentSummary` 增加可空 `media_type`，但不增加宽高或图片类型字段；消息投影继续保存
`attachment_ids`。Desktop 对 `media_type = image/*` 的 ready 附件请求缩略图资源；旧附件 MIME
为空时允许尝试请求。Host 仍以实际内容为准，成功时展示图片样式，返回不支持媒体时回退到现有普通
附件样式。

用户消息正文上方渲染有界缩略图网格。为避免历史消息把完整 16 MB Data URL 全部加载进 DOM，Host
新增只读、鉴权的缩略图资源入口：优先读取 Blob 同目录的 `<原 Blob 文件名>.thumbnail.jpg`，缺失时
按最大边 320 px 生成后原子保存；Tauri native bridge 转成短生命周期 Data URL/Blob URL，列表离开
视口后释放 URL。

点击缩略图继续调用现有 `preview_attachment` 并复用 `AttachmentPreviewDialog`，不创建第二套预览
弹窗。预览和缩略图都只允许当前 Session 可见的 ready attachment。

Session View 增加识图能力投影：`native`、`tool` 或 `unavailable`，并携带不可用原因枚举。Composer
只展示提示，不自己根据 Provider/model name 推导能力。

## 八、验证矩阵

| 层级 | 必测内容 |
| --- | --- |
| 类型 | 统一 FileReference 保持不变、AttachmentSummary 可空 MIME、混合顺序、旧引用兼容 |
| Host | MIME 检测、公共预处理、实时尺寸提取、伪扩展名、损坏图、像素/字节上限、取消 |
| Store | `attachment_blobs.media_type` 迁移、新上传写入、旧值 NULL、无图片宽高或缩略图路径字段 |
| Blob | `<原 Blob 文件名>.thumbnail.jpg` 命名、原子生成、去重复用、缺失重建、随原图清理 |
| Adapter | 四家相同 JPEG 输入的图片 JSON golden、无 Provider 专属转换、reasoning、tool 链、日志脱敏 |
| Runtime | 原生/工具/不可用三路判定、多图、混合附件、配置 reload、辅助模型失效 |
| Tool | Schema、一次模型调用、无嵌套工具、timeout/retry/cancel、usage 分栏 |
| Desktop | 消息上方缩略图、懒加载释放、现有预览弹窗、工具卡片、能力提示 |
| 真实模型 | OpenAI-compatible 视觉、至少一个 GLM/Qwen/Kimi 协议、文本主模型 + 辅助视觉 |

必须增加一项防泄漏回归：扫描 SQLite、Conversation JSONL、配置诊断、模型观察事件和错误文本，确认
不存在 `data:image/` 或大段 Base64 正文。
