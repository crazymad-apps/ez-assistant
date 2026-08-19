# v0.16.0 技术方案附件 B：会话 effort、协议与桌面交互

## 一、附件信息

- 状态：已确认（2026-08-18）
- 主方案：[`v0.16.0 技术方案`](技术方案.md)
- 功能基线：[`v0.16.0 功能设计`](功能设计.md)
- 协议映射：[`附件 A：多模态与模型协议`](技术方案附件A-多模态与模型协议.md)

本文只处理产品稳定 effort、Session/Run 状态、协议投影、模型切换和 Composer 交互。Provider 原生
字段、thinking 开启和 reasoning 历史回放以附件 A 为准。

## 二、领域模型

### 2.1 稳定 key

扩充 `agent-model::ReasoningEffort`：

```rust
pub enum ReasoningEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}
```

枚举顺序不是隐式业务规则；实现显式提供 `rank()` 或 `Ord`，固定为
`low < medium < high < xhigh < max`。序列化值使用小写稳定 key。

“默认”使用 `Option<ReasoningEffort>::None`，含义是“Session 没有保存显式强度，由模型协议选择
默认开启档位”。Provider 默认已经开启思考时可以省略强度；Provider 的原生默认会关闭思考时，协议
必须显式编码一个有效开启档位。它不是关闭 thinking，也不参与向下档位比较。

### 2.2 有效选项

内部把实际可选项投影为：

```rust
pub struct ReasoningEffortOption {
    pub key: ReasoningEffort,
    pub label: String,
}
```

- key 用于 Session 持久化、排序和模型切换降级；
- label 由当前模型的 `effort_map` 提供，Desktop 原样显示；
- wire value 来源于静态目录或高级 override 的 `effort_map`，配置编译后只留在 Adapter 私有映射中，
  不进入 Session/Run 状态或应用协议；
- 同一模型中两个稳定 key 映射到相同实际强度时配置编译失败，不在运行时去重或猜测保留项；
- 选项列表为空表示不提供 effort 配置，整个选择器不显示。

OpenAI 兼容基准保留五个 active key。协议可以只公布子集，例如 GLM 只公布“高”和“最大”，
Kimi K3 公布 `low`/`high`/`max`，K2.x 列表为空。不能为保持 UI 对称而显示不可用项或禁用项。

## 三、配置编译

Runtime Host 的静态模型目录按精确 `(provider, protocol, model_id)` 提供 capability；正常用户不在
模型设置页编辑 capability。配置编译按以下步骤执行：

1. 从模型配置读取实际 `provider`、`protocol` 和 `model_id`；`provider` 只表示推理服务，`protocol`
   表示协议实现，两者共同参与匹配。
2. 先应用用户模型配置中的 capability override；reasoning 块只能整体覆盖，不能逐字段混合来源。
3. 对仍未指定的能力读取随包静态模型目录中的精确三元组条目；`effort_map` 的 key 直接形成有效
   effort 列表，不再读取另一份 `reasoning_efforts`；不按品牌或模型名称前缀模糊匹配。
4. 目录未命中时保留当前 `protocol` 并使用该协议的保守能力基线，reasoning effort 列表默认为空；
   `protocol` 没有 Adapter 实现时配置编译失败。
5. 根据 `provider + protocol` 构造具名协议 Adapter，校验 `effort_map` 只含稳定 key、label 非空、
   wire value 类型和范围合法，并且不同 key 不映射到重复的实际强度；不支持时配置编译失败，不静默
   删除或改档。
6. 目录 `default_effort` 必须存在于 `effort_map`，并编译成
   `default_active_reasoning_effort`；没有 effort 概念时由协议 Adapter 的独立 thinking 开启字段保证
   默认开启。
7. 生成内部 `ModelCapabilities` 和面向 Runtime 的有效 options；只有完整编译成功才替换配置快照。

模型设置页不显示 capability、thinking 或 effort 默认值；用户选择入口只在会话 Composer。高级用户
override 只用于自定义 Endpoint、目录尚未收录的新模型或兼容排障，并且优先于静态目录。

## 四、Session 与 Run 持久化

### 4.1 SQLite 字段

`sessions` 增加：

```text
reasoning_effort TEXT NULL
CHECK (reasoning_effort IS NULL OR reasoning_effort IN
       ('low', 'medium', 'high', 'xhigh', 'max'))
```

`runs` 增加同样的可空字段。两者意义不同：

- `sessions.reasoning_effort`：当前 Session 对后续 Run 的用户意图；
- `runs.reasoning_effort`：该 Run 真正启动时解析并冻结的有效档位；Session 为默认且模型 capability
  具有 `default_active_reasoning_effort` 时写入该具体 key。只有模型没有 effort 概念时为空，由冻结
  协议的 thinking 开启字段保证思考开启。

旧行自然为 `NULL`，无需回填。Fork Session 继承源 Session 的 model key 和 effort；因为模型相同，
无需再次降级。迁移仍需遵循主方案定义的数据库备份与人工确认门禁。

### 4.2 Session 内存状态

`StoredSession`、`NewStoredSession`、`SessionState`、`SessionSummary` 增加可空 effort。新建 Session
默认为空。Runtime 启动恢复时执行防御性校验：若数据库值不在当前模型有效选项中，内存和持久化都
不能静默保留失效值；先记录诊断，并通过一次明确的修复存储操作降级到不高于旧值的最高有效档，
无有效档则清空。该写操作进入迁移/恢复测试并记录影响行数。

为减少自动批量修复，正常配置 reload 不立即改写所有 Session。Session 第一次加载/打开或运行前做
同样的单 Session 校验；用户可见投影永远不返回一个 options 中不存在的 active key。

### 4.3 Run 启动冻结

“启动”定义为 Session 驱动器成功 claim 持久化输入、准备把对应 Run 从 queued/accepted 推向
running 的边界，而不是用户点击发送的时刻。这样排队期间修改 effort 能作用于尚未启动的 Run。

Runtime 先把 Session 选择解析为有效 Run 值：显式选择直接使用；默认状态使用模型 capability 的
`default_active_reasoning_effort`；无 effort 概念则为空。Store 再执行带期望状态的原子操作：

```text
start_run(run_id, expected_status, frozen_reasoning_effort, started_at)
```

只有状态仍可启动时才同时写入 running 和冻结值；影响行数不是 1 即停止执行。Runtime 随后编译
Agent 时只读取 Run 冻结值，不再次读取 Session effort。重试创建新 Run，并在新 Run 启动时重新
冻结当时 Session 值。

上下文压缩、工具继续或同一 Run 内重新装配模型请求都必须沿用 Run 冻结值；不得在长 Run 中途因
用户改了 Composer 选择而改变强度。

## 五、Runtime 命令

### 5.1 设置 effort

新增应用命令：

```rust
pub struct SetSessionReasoningEffortRequest {
    pub session_id: SessionId,
    pub effort: Option<ReasoningEffortKey>,
}
```

Runtime 在 Session mutation gate 内完成：

1. 读取当前 Session model 和当前配置快照；
2. `Some` 必须存在于该模型有效 options，`None` 始终可接受；
3. 先持久化，再更新内存，并增加 Session/应用投影 revision；
4. 返回最新 `SessionSummary` 或 `SessionViewSnapshot`。

effort 变更允许在 Run 执行期间发生，因为当前 Run 已冻结；UI 明确提示“用于后续运行”。归档或已
删除 Session 仍拒绝修改。并发请求使用既有 mutation gate 和 revision 语义，不由 Desktop 猜最终值。

### 5.2 模型切换的原子降级

现有 `ModelChange` 替换为同时表达两个字段的存储契约，例如：

```rust
pub struct SessionModelConfigurationChange {
    pub session_id: SessionId,
    pub expected_model_key: String,
    pub model_key: String,
    pub reasoning_effort: Option<ReasoningEffort>,
}
```

Runtime 在同一配置快照中计算新值：

```text
old = None                     -> None
old 在新 options 中             -> old
存在 key <= old                -> 其中 rank 最大者
否则                            -> None
```

然后以一个 Store 操作同时更新 `model_key` 和 `reasoning_effort`。禁止先改模型、再异步修 effort；
任一步失败都保持原值。当前既有规则仍要求 Session idle/empty 才可切换模型，本版本不放宽。

### 5.3 配置 reload

reload 可以让某模型的 options 改变。Runtime 不阻塞整个配置替换去批量写所有 Session，而是：

- 新配置快照先通过完整编译；
- 当前打开 Session 的投影刷新，并在其 mutation gate 中执行必要降级；
- 未打开 Session 在下次加载/Run 启动前校验；
- 已经 running 的 Run 继续使用冻结的旧 key 和其已装配服务，不受 reload 影响；
- 若旧服务快照已不可用，沿用现有 config snapshot 生命周期保证 Run 完成，不中途改协议。

## 六、应用协议投影

### 6.1 DTO

`assistant-protocol` 使用自己的轻量枚举/字符串 DTO，不依赖 `agent-model`：

```rust
pub enum ReasoningEffortKey {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

pub struct ReasoningEffortOptionSnapshot {
    pub key: ReasoningEffortKey,
    pub label: String,
}

pub enum ImageHandlingMode {
    Native,
    Tool,
    Unavailable,
}

pub struct ComposerCapabilitiesSnapshot {
    pub reasoning_effort_options: Vec<ReasoningEffortOptionSnapshot>,
    pub image_handling: ImageHandlingMode,
    pub image_unavailable_reason: Option<ImageUnavailableReason>,
}
```

`SessionSummary` 增加当前 `reasoning_effort`；`SessionViewSnapshot` 增加当前模型编译后的 Composer
capabilities。options 属于“当前 Session + 当前配置”的派生投影，不持久化到 Session，也不放到
全局 Model Summary 让 Desktop 二次组合。

`RunSnapshot` 增加冻结 effort；queued/尚未启动的 Run 为 `None` 可能同时表示尚未冻结或默认，因此
如 UI 需要区分，应以 Run 状态判断，不再增加重复布尔值。

### 6.2 事件与生成类型

effort 成功变更、模型切换降级和配置 reload 触发既有 Session/应用投影变化事件。事件只是失效通知，
Desktop 仍以重新获取的组合快照为权威。所有 DTO 由 `ts-rs` 重新生成
`apps/desktop/src/generated/assistant-protocol.ts`，Rust round-trip 和 generated diff 都进入门禁。

## 七、Desktop Composer 交互

### 7.1 选择器位置与可见性

effort selector 放在会话输入框工具区，与当前模型、Plan/Build、审批模式属于同一组会话配置；不放
进模型设置页的用户偏好区域。

- `reasoning_effort_options` 为空：整个 selector 不渲染。
- 非空：选项第一项是“默认”，随后按 Runtime 返回顺序展示协议 label。
- 当前值使用 stable key 匹配；Desktop 不保存 Provider wire value，也不自行补全五档。
- 模型切换后直接使用 Runtime 返回的新当前值和 options；如果发生降级，可以显示一次非阻塞提示，
  如“当前模型已将推理强度调整为高”，但不显示“OpenAI key -> 协议值”的内部映射。

### 7.2 修改时机

选择后立即发送 `set_session_reasoning_effort`，不等到 submit input 一起提交。请求期间仅禁用 selector
自身；失败时恢复 Runtime 最新投影并显示内联错误。正在运行时可修改，控件辅助文本/tooltip 表明
“应用于后续运行”。

不在每条消息上显示 effort badge；Run 详情/诊断可展示冻结值以便核查。thinking 仍默认开启，本版本
不出现开关。

### 7.3 可访问性与布局

- 使用现有可访问 Listbox/Popover 原子组件和键盘导航，不自制不可聚焦菜单。
- accessible name 使用“推理强度”；协议 label 是选项名称。
- 窄宽 Composer 先收进更多菜单，但仍属于 Composer，不迁移到设置页。
- 选项切换不移动输入焦点，不清空 draft，不触发发送。

## 八、测试矩阵

### 8.1 领域与存储

- 五档序列化、显式排序和 `None` 语义；
- 新 Session 默认、Fork 继承、旧 DB NULL 兼容；
- Run claim 与 frozen effort 原子提交、失败影响行数门禁；
- 运行中修改只影响下一 Run；压缩/工具继续仍使用冻结值；
- reload 后单 Session 懒校验和无批量改写。

### 8.2 模型切换

| 原值 | 新模型 options | 结果 |
| --- | --- | --- |
| 默认 | 任意 | 默认 |
| `high` | `low, high, max` | `high` |
| `xhigh` | `low, high, max` | `high` |
| `medium` | `high, max` | 默认，不向上选 `high` |
| `max` | 空 | 默认，selector 隐藏 |

另测 Store 失败时 model 与 effort 均不变、配置 revision 并发变化时重新校验、Desktop 不出现旧隐藏值。

### 8.3 Provider/UI

- OpenAI 五档、GLM 有效子集、Kimi K3 三档、Qwen 与 Kimi K2.x 无映射时不显示；
- 协议 label 原样呈现，wire value 不进入应用协议；
- selector 显示/隐藏、默认项、运行中修改、失败回滚、窄宽与键盘操作；
- 请求 golden 确认 Session key 被 Adapter 正确转换且 thinking 始终开启。
