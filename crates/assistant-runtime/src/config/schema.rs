//! `config.toml` schema version 1 的 serde 输入类型。
//!
//! 模型表先保留为 [`toml::Value`]，由编译层逐条反序列化，确保单模型字段错误不会
//! 阻止其他模型进入有效配置。
//!
//! Raw 类型只描述文件形状，不承担业务默认合并、范围判断或安全投影，也不 derive Debug/
//! Serialize。尤其是 `RawModelConfig` 可以短暂持有 API Key，不能作为 Runtime 状态长期保存。

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// schema version 1 的顶层输入。
///
/// 未知全局字段直接失败；models 保留 value map 是唯一有意延迟的反序列化边界。
pub(super) struct RawConfig {
    /// 决定整份文件的解释规则。
    pub(super) schema_version: u32,
    /// 用户选择的默认模型 key，形式和可用性在语义阶段分别校验。
    pub(super) default_model: String,
    /// Runtime 维度配置；整表缺失时使用明确默认值。
    #[serde(default)]
    pub(super) runtime: RawRuntimeConfig,
    /// Agent 维度配置；整表缺失时不注入 generation 或执行上限。
    #[serde(default)]
    pub(super) agent: RawAgentConfig,
    /// 按用户 key 保存的模型原始表；每个 value 后续独立编译。
    #[serde(default)]
    pub(super) models: BTreeMap<String, toml::Value>,
}

/// Runtime 统一拥有的模型传输与建立重试配置。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRuntimeConfig {
    #[serde(default)]
    pub(super) model_transport: RawModelTransportConfig,
    pub(super) model_retry: Option<RawModelRetryConfig>,
}

/// 模型 HTTP 建连、响应建立和流空闲的超时输入，单位统一为毫秒。
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawModelTransportConfig {
    pub(super) connect_timeout_ms: u64,
    /// 等待响应建立及相邻流 chunk 的最长时间；不是流式请求总时长上限。
    pub(super) request_timeout_ms: u64,
}

impl Default for RawModelTransportConfig {
    /// 使用技术方案确认的显式缺省值，避免依赖 HTTP client 自身可能变化的默认行为。
    fn default() -> Self {
        Self {
            connect_timeout_ms: 10_000,
            request_timeout_ms: 300_000,
        }
    }
}

/// 显式有限重试表；整表不存在与字段为空具有不同语义。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawModelRetryConfig {
    pub(super) retry_on: Option<Vec<String>>,
    pub(super) delays_ms: Option<Vec<u64>>,
    pub(super) max_retry_after_ms: Option<u64>,
}

/// Agent 配置根；当前只包含所有模型共享的默认执行配置。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAgentConfig {
    #[serde(default)]
    pub(super) defaults: RawAgentDefaults,
}

/// 每个新 Run 编译时使用的 Agent 默认值集合。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawAgentDefaults {
    #[serde(default)]
    pub(super) generation: RawGenerationConfig,
    #[serde(default)]
    pub(super) execution_limits: RawExecutionLimits,
    #[serde(default)]
    pub(super) guardrails: RawGuardrailConfig,
    #[serde(default)]
    pub(super) delegation: RawDelegationConfig,
}

/// 单层子任务委派的模型无关调度与执行上限。
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawDelegationConfig {
    pub(super) max_tasks_per_run: u32,
    pub(super) max_concurrent_tasks: u32,
    pub(super) task_timeout_ms: u64,
    pub(super) max_steps: u32,
    pub(super) max_tool_calls: u32,
    pub(super) max_output_tokens: u32,
}

impl Default for RawDelegationConfig {
    fn default() -> Self {
        Self {
            max_tasks_per_run: 8,
            max_concurrent_tasks: 4,
            task_timeout_ms: 900_000,
            max_steps: 40,
            max_tool_calls: 100,
            max_output_tokens: 16_384,
        }
    }
}

/// Provider-neutral generation 输入；缺失字段保留 Provider 默认行为。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawGenerationConfig {
    pub(super) temperature: Option<f32>,
    pub(super) top_p: Option<f32>,
    pub(super) max_output_tokens: Option<u32>,
    #[serde(default)]
    pub(super) stop: Vec<String>,
}

/// Agent Loop 的资源预算输入，不表示模型能力上限。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawExecutionLimits {
    pub(super) max_steps: Option<u32>,
    pub(super) max_tool_calls: Option<u32>,
}

/// Agent Loop 的模型无关 Guardrail 输入。
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawGuardrailConfig {
    pub(super) repeated_invocation: RawGuardrailCheck,
    pub(super) consecutive_failures: RawGuardrailCheck,
}

impl Default for RawGuardrailConfig {
    fn default() -> Self {
        Self {
            repeated_invocation: RawGuardrailCheck {
                mode: RawGuardrailMode::Enforce,
                threshold: 4,
            },
            consecutive_failures: RawGuardrailCheck {
                mode: RawGuardrailMode::Enforce,
                threshold: 5,
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawGuardrailCheck {
    pub(super) mode: RawGuardrailMode,
    pub(super) threshold: u32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RawGuardrailMode {
    Off,
    Observe,
    Enforce,
}

/// 单个 `models.<key>` 的原始字段集合。
///
/// 必填字段仍使用 Option，是为了在语义阶段一次报告多个缺失项，而不是让 serde 在第一个字段
/// 处提前返回；未知字段和错误字段类型仍由 deny_unknown_fields/serde 拒绝。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawModelConfig {
    pub(super) display_name: Option<String>,
    pub(super) protocol: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) endpoint: Option<String>,
    pub(super) model: Option<String>,
    pub(super) api_key: Option<String>,
    pub(super) context_window_tokens: Option<u64>,
    pub(super) max_output_tokens: Option<u32>,
}
