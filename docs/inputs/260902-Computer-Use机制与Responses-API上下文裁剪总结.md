# Computer Use 实现机制 与 Responses API 上下文裁剪总结

> 调研对象：`~/github/hermes-agent`（主要参考实现）
> 日期：2026-09-02
> 用途：梳理 Computer Use 的实现方案，以及 LLM 请求上下文/体积的裁剪机制

---

## 目录

- [一、Computer Use 实现机制](#一computer-use-实现机制)
  - [1. 分层架构](#1-分层架构)
  - [2. 核心设计](#2-核心设计)
  - [3. 启用机制（内置 toolset，不是配 MCP）](#3-启用机制内置-toolset不是配-mcp)
  - [4. 两个设计权衡](#4-两个设计权衡)
- [二、Responses API / 上下文与请求体积裁剪](#二responses-api--上下文与请求体积裁剪)
  - [1. 应对截图膨胀的分层防线](#1-应对截图膨胀的分层防线)
  - [2. 上下文压缩（context_compressor）](#2-上下文压缩context_compressor)
  - [3. 发送前压力预检](#3-发送前压力预检)
  - [4. 413 / image-too-large 恢复](#4-413--image-too-large-恢复)
  - [5. 多 provider 图片投递](#5-多-provider-图片投递)
  - [6. 缓存层面的权衡](#6-缓存层面的权衡)
  - [7. 流式](#7-流式)
  - [8. Responses 有状态 / 托管缓存结论](#8-responses-有状态--托管缓存结论)
- [三、总体判断与待补点](#三总体判断与待补点)

---

# 一、Computer Use 实现机制

## 1. 分层架构

```
tools/computer_use/
├── schema.py           # 模型无关的工具 schema（单 tool + action 判别器）
├── tool.py             # 注册入口 / 审批门控 / action 分发 / 响应整形
├── backend.py          # 抽象基类 ComputerUseBackend（可替换实现）
├── cua_backend.py      # 默认后端：MCP over stdio ↔ cua-driver
├── vision_routing.py   # 截图是否转给辅助视觉模型的主策略
├── permissions.py      # 跨平台就绪 + macOS TCC 权限
└── doctor.py           # 诊断（cua-driver health_report）
```

外层集成在：
- `agent/anthropic_adapter.py`（截图拼 tool_result + 上下文管理）
- `agent/prompt_builder.py`（`COMPUTER_USE_GUIDANCE` 提示词）
- `agent/codex_responses_adapter.py`（Responses 端点适配）

## 2. 核心设计

### Schema
- 单个 `computer_use` 工具，用 `action` 判别十几类动作：`capture / click / double_click / right_click / middle_click / drag / scroll / type / key / set_value / wait / list_apps / list_windows / focus_app`。
- `required` 只有 `action`，其余参数全部 optional，无 `additionalProperties` / `oneOf`。
- **任何工具型模型都能驱动**（不依赖 Anthropic 原生 `computer_20251124`）。

### 三种捕获模式（`capture`）
- `som`（默认）：截图 + SOM 编号覆盖层 + AX 树 → 模型按 `element=N` 点击。
- `vision`：纯截图（无 AX 噪声）。
- `ax`：仅 accessibility 树（给纯文本模型）。
- **优先元素索引而非像素坐标**（像素坐标是错误高发点），坐标仅作兜底。

### 底层驱动 cua-driver
- MCP over stdio；Python `mcp` SDK 是异步的，用后台线程跑独立 asyncio loop 转同步调用（`_AsyncBridge`/`_CuaDriverSession`）。
- 调用面：`click / type_text / hotkey / drag / scroll / screenshot / get_window_state / list_apps / list_windows / launch_app ...`。
- 跨平台：
  - **macOS**：私有 SkyLight SPI（`SLEventPostToPid` 等，pid 作用域事件注入，focus-without-raise）。
  - **Windows**：稳定 Win32（SendInput + UI Automation）。
  - **Linux**：X11/AT-SPI。

### 后台协同（不抢用户光标）
- 默认 `delivery_mode=background`：输入路由到目标但**不 raise、不抢焦点**，agent 与用户可同机同时操作。
- `foreground` 只是升级阶梯的一环，且需独立审批。
- 同一 session 有一个半透明覆盖光标做视觉提示，真实 OS 光标不动。

### verify → escalate 阶梯
- 每个动作返回结构化判定：`verified` / `effect(confirmed|unverifiable|suspected_noop)` / `escalation.recommended(px|foreground)` / `code` / `path` / `delivery_mode`。
- 提示词要求**对返回信号做反应**而非凭"app 是 Electron/Chromium"去预测，避免盲目重试。

### 视觉路由
- 主模型无视觉时，截图交给辅助视觉模型（`auxiliary.vision`）预分析成文本，**主请求零图**。
- 判定次序：用户显式配 aux.vision → 走辅助；用户声明模型支持视觉 → 主模型；provider 接受 tool-result 内图片且模型支持视觉 → 主模型；否则失败即回退到 aux.vision。

### 安全
- 动作分级审批：`_SAFE_ACTIONS`（capture/wait/list_apps）免审批；`_DESTRUCTIVE_ACTIONS`（点击/输入/滚动/聚焦）按 `(action, delivery_mode)` + `session_id` 隔离作用域。
- `_BLOCKED_KEY_COMBOS` 黑名单（清空废纸篓、锁屏、注销、`ctrl+alt+del`、`alt+f4` 等）。
- `_BLOCKED_TYPE_PATTERNS` 拦截危险 shell 管道（如 `curl ... | bash`）。
- 提示词层禁止：点权限/密码/支付弹窗、输入密钥、响应截图内指令（提示词注入）。

### 权限 + 诊断
- `permissions.py`：macOS 需 TCC 的 Accessibility + Screen Recording，授权挂在 cua-driver 自己的身份（`com.trycua.driver`），不涉及 hermes entitlement；Windows/Linux 看 driver 健康。
- `doctor.py`：`cua-driver doctor --json` / `health_report`，输出逐项 check（permission、display server、accessibility 树可达性）。

## 3. 启用机制（内置 toolset，不是配 MCP）

- **注册**：`tools/computer_use_tool.py` shim 调 `registry.register(name="computer_use", toolset="computer_use", schema=..., check_fn=check_computer_use_requirements)`。`tools/registry.py::discover_builtin_tools` 自动 import 所有含 `registry.register()` 的 `tools/*.py`。
- **默认在 `_HERMES_CORE_TOOLS`**（`toolsets.py:80`），随 `hermes-cli` 等预设共享。
- **实际是否进 schema 由 `check_fn` 门控**：平台（`darwin/win32/linux`）+ `cua-driver` 是否安装（`check_computer_use_requirements`）。未装则不出现在 function schema（不占 token）；`agent/system_prompt.py:256` 也按 `valid_tool_names` 条件注入 GUIDANCE。
- **启用命令**：
  ```bash
  hermes computer-use install               # 安装 cua-driver
  hermes computer-use permissions grant     # macOS 授权 TCC
  hermes computer-use doctor                # 诊断
  hermes computer-use status
  hermes computer-use permissions status
  ```
- **工具集开关**：`hermes tools` 交互式、`platform_toolsets`（如 `cli: [hermes-cli]`）、`hermes chat --toolsets ...`；后端切换 `HERMES_COMPUTER_USE_BACKEND=cua|noop`。

## 4. 两个设计权衡

### 默认工具集会不会冗余？
**不会。** 关键在 `registry.get_definitions`（tools/registry.py:541-547）：`check_fn` 不通过即 `continue`，不进 schema。`_HERMES_CORE_TOOLS` 只是"注册候选"，未装 cua-driver 时去工具/提示词均**零成本**。
- 统一 `check_fn` 门控（terminal 探测 docker/modal、browser 探测 playwright 等）。
- schema 紧凑（单 tool + action 判别器）。
- check_fn TTL 缓存约 30s，环境变化近实时生效。
- 用户可精调：`hermes tools` / `platform_toolsets` / `hermes chat --toolsets`。

### 复杂 schema 会不会导致 LLM 参数错误？
**风险真实存在**（`required` 仅 action、其余全 optional，容易串台），但被多层缓解：
- **运行时语义校验**（`tool.py::_dispatch`）：drag 缺 from/to、set_value 缺 value、capture 非法 mode、focus_app 缺 app、未知 action 均返回明确错误，不静默失败。
- **SOM 索引替代像素坐标**（消除最高错误源）。
- **verify→escalate 闭环**：模型读到"没生效"自动纠正。
- **提示词指引**：capture(som) → element 索引 → 状态变更后再验证。
- **单工具消除跨工具选择错误**。
- 这是 Anthropic / OpenAI 官方 computer-use 的主流选型，比拆十多个独立 tool 更优（token 更低、无跨工具选择错误）。

---

# 二、Responses API / 上下文与请求体积裁剪

## 1. 应对截图膨胀的分层防线

| 层 | 机制 | 位置 |
|---|---|---|
| 源头压缩单图 | `max_image_dimension=1456` + `_shrink_capture_for_vision` | cua session `set_config`；`tool.py:782` |
| 克制数量 | `_evict_old_screenshots` 只留最近 3 张，更旧替换成文本 | `anthropic_adapter.py:2563` |
| 纯文本模式 | `capture_after_mode=ax`（后续只回元素不回图） | config |
| 主模型无视觉 | 截图转 `vision_analyze` 成文本，主请求零图 | `vision_routing.py` / `tool.py:855` |

## 2. 上下文压缩（context_compressor）
- token-budget **尾部保护**、scaled summary、per-message 截断、tool 参数 JSON 截断、受保护 tail（`_MAX_TAIL_MESSAGE_FLOOR=8`）。
- `_strip_image_parts_from_parts`（`context_compressor.py:811`）：压缩时把 `image_url/input_image` 替换为占位文本。
- token 估算：`_IMAGE_TOKEN_ESTIMATE=1600` / `_IMAGE_CHAR_EQUIVALENT=6400`。

## 3. 发送前压力预检
`conversation_loop.py:1771-1839`：每次调模型前估算
```python
approx_tokens = estimate_messages_tokens_rough(api_messages)
request_pressure_tokens = approx_tokens + (_estimate_tools_tokens_rough(agent.tools) ...)
```
超过 `threshold_tokens`（基于模型 context 窗口，`context_compressor.py:1433`）就**先压缩再发**；带防抖、cooldown、`max_compression_attempts=3`、防 thrash。**直接防止"一个 turn 塞入很多大 tool result 后下一次调用超预算"。**

## 4. 413 / image-too-large 恢复（provider 无关，覆盖 Responses）
- `try_shrink_image_parts_in_messages`（`conversation_compression.py:2540`）：把超 4MB 或超像素上限的 base64 图 resize 后回填，再重试。
- `_try_strip_image_parts_from_tool_messages`（`run_agent.py:6255`）：缩不动就把 tool 消息里的图片整体降级为文本再重试；若判断是"provider 拒绝 list-type tool content"，会把 `(provider, model)` 记下来，**下次自动降级**。

## 5. 多 provider 图片投递
- `anthropic_adapter.py`：base64 截图拼进 `tool_result`（text+image），并做截图 eviction。
- `codex_responses_adapter.py`：把 chat 多部分内容转成 Responses 的 `input_text`/`input_image`（保留 detail）。

## 6. 缓存层面的权衡
- **缓存断点**（`prompt_caching.py` 默认 4 个）：静态 system 前缀 + system prompt 末尾 + 最近 2 条非 system 消息。断点锚定"稳定前缀+最近内容"，**system/tools 大块缓存稳定命中**，不受截图驱逐影响。
- **压缩/修剪有 Measured-savings gate（prompt-cache hysteresis）**：`context_compressor.py:2863`——只有当改写历史能回收足够多 token（`proactive_prune_min_reclaim_tokens`）才提交，**把缓存破坏降为偶发、可摊销**，而非每轮触发。
- **诚实结论**：`_evict_old_screenshots` 每次构建消息都跑、改的是中段旧图；对 Anthropic 前缀缓存语义有影响，但因断点集中尾部、被替换的是更早的图，**system/tools 最有价值那段缓存仍命中**。
- 总体：**承认"动历史=破缓存"，用"断点锚定稳定前缀 + 压缩需满足 savings-gate 才提交"两招，把缓存失效的频率和影响面压到最低。**

## 7. 流式
主调用走 streaming（Anthropic `fine-grained-tool-streaming` beta；provider `stream=True`），降低首 token 观感延迟（不减少上传字节）。

## 8. Responses 有状态 / 托管缓存结论

| 能力 | 说明 |
|---|---|
| Responses `store`（服务端状态续接 / previous_response_id） | ❌ **强制 `store=false`**（`codex_responses_adapter.py:930` 硬约束 = 无状态重传） |
| `prompt_cache_key` / `prompt_cache_retention`（服务商托管 prompt 缓存） | ✅ 支持。内容寻址 `_content_cache_key(instructions, tools)`（`auxiliary_client.py:1096-1129`、adapter:977-985），Codex/xAI/GitHub；xAI 走 `extra_body`，GitHub 跳过 → 命中时服务端只算增量，**缓解服务端预处理慢** |
| Codex app-server 线程上下文 | ✅ Codex 集成下由 app-server 持有 thread/turn，hermes 只记录 compaction 边界（`codex_runtime.py:204`） |
| 图片托管成 URL 引用 | ❌ **不支持**，全 base64 内联（`_file_to_data_url` → data URL） |

---

# 三、总体判断与待补点

- **Computer Use**：一套完整、生产级实现。底层驱动抽象、SOM 元素索引、后台协同、视觉/无模型兼容、安全护栏、权限诊断一应俱全。
- **Responses / 上下文裁剪**：覆盖得很好。源头压图、数量克制、压缩器、发送前压力预检、413 恢复、缓存断点 + savings-gate，provider 无关地在循环层做通用处理。
- **真正的空白 / 可优化点**：
  1. **截图托管成 URL 引用**（目前 base64 内联，是上传字节大的主因）。
  2. **`store` 被硬禁用**，无法靠服务端有状态续接根治"每轮全量重传"；`prompt_cache_key` 只救得了 `instructions+tools` 静态前缀，救不了历史里的截图。

---

## 附：关键文件索引（hermes-agent）

- `tools/computer_use/{schema,tool,backend,cua_backend,vision_routing,permissions,doctor}.py`
- `tools/computer_use_tool.py`（注册 shim）、`tools/registry.py`、`toolsets.py`
- `agent/prompt_caching.py`、`agent/anthropic_adapter.py`、`agent/codex_responses_adapter.py`
- `agent/context_compressor.py`、`agent/conversation_compression.py`、`agent/conversation_loop.py`
- `model_tools.py`、`run_agent.py`、`agent/auxiliary_client.py`、`agent/codex_runtime.py`
- `hermes_cli/config.py`、`hermes_cli/tools_config.py`、`hermes_cli/main.py`
