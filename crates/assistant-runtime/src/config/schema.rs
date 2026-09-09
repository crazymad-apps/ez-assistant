//! 全局 config.toml 输入；服务商、模型引用和固定参数由数据库模型管理承担。

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
/// schema version 1 的顶层输入。
///
/// 未知全局字段直接失败；Host 自有表由各自模块解释。
pub(super) struct RawConfig {
    /// 决定整份文件的解释规则。
    pub(super) schema_version: u32,
    /// Runtime 维度配置；整表缺失时使用明确默认值。
    #[serde(default)]
    pub(super) runtime: RawRuntimeConfig,
    /// Agent 维度配置；整表缺失时不注入 generation 或执行上限。
    #[serde(default)]
    pub(super) agent: RawAgentConfig,
    /// MCP 进程级运行参数；逐 Server 配置存放在独立的 `mcp.json` 中。
    #[serde(default)]
    pub(super) mcp: RawMcpConfig,
    /// Host 私有语音配置由 Runtime Host 使用同一安全配置源解释；Runtime 只容忍该顶层表，
    /// 不编译、保存或投影其中的 Provider 与 credential。
    #[serde(default, rename = "speech")]
    pub(super) _host_speech: Option<toml::Value>,
    /// Host 自行解释访问控制；密码哈希不进入 Runtime 配置投影。
    #[serde(default, rename = "host_access")]
    pub(super) _host_access: Option<toml::Value>,
}

/// Runtime 统一拥有的模型传输与建立重试配置。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawRuntimeConfig {
    #[serde(default)]
    pub(super) model_transport: RawModelTransportConfig,
    pub(super) model_retry: Option<RawModelRetryConfig>,
}

/// MCP 调用缺省时限，以及连接、目录、关闭和并发的进程级上限。
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct RawMcpConfig {
    pub(super) connect_timeout_ms: u64,
    pub(super) catalog_timeout_ms: u64,
    pub(super) request_timeout_ms: u64,
    pub(super) close_timeout_ms: u64,
    pub(super) max_concurrent_calls_per_server: u32,
}

impl Default for RawMcpConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 15_000,
            catalog_timeout_ms: 30_000,
            request_timeout_ms: 120_000,
            close_timeout_ms: 5_000,
            max_concurrent_calls_per_server: 8,
        }
    }
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
    /// 辅助识图的请求预算；模型引用由数据库设置提供。
    pub(super) vision: Option<RawVisionConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawVisionConfig {
    pub(super) timeout_ms: u64,
    pub(super) max_output_tokens: u32,
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
