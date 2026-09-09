//! 配置编译产生的领域状态、有效快照和脱敏投影。
//!
//! 同一份用户配置在 Runtime 内有两个严格分离的视图：`ResolvedConfig` 服务于执行装配，
//! 可以持有 secret；`ConfigProjection` 服务于查询和未来应用协议，只能包含脱敏事实。
//! 两者不通过通用 Serialize 相互转换，避免新增字段时意外把 credential 带出进程边界。

use std::{fmt, time::Duration};

use agent_core::{ExecutionBudget, GuardrailConfig};
use agent_model::{GenerationConfig, ModelRetryPolicy};
use agent_types::ProviderId;

use super::source::ConfigSourceFailureKind;

/// 当前配置源可供 Runtime 使用的程度。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigState {
    /// 配置文件不存在。
    Missing,
    /// 配置无法形成任何可用快照。
    Invalid,
    /// 已形成快照，但部分模型或默认模型不可用。
    Degraded,
    /// 默认模型和全部模型配置均有效。
    Ready,
}

/// 配置诊断的稳定内部分类；具体协议 DTO 在协议里程碑中单独定义。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigIssueCode {
    /// TOML 语法或重复字段错误。
    InvalidSyntax,
    /// 不支持的 schema version。
    UnsupportedSchemaVersion,
    /// 顶层或全局表结构无法解释。
    InvalidTopLevel,
    /// 敏感配置源的文件类型、权限或大小不安全。
    UnsafeConfigSource,
    /// 敏感配置源无法读取或解码。
    ConfigReadFailed,
    /// 存在未知字段。
    UnknownField,
    /// 必填字段缺失。
    MissingField,
    /// token、超时或执行上限无效。
    InvalidLimit,
    /// Runtime/Agent 策略无效。
    InvalidPolicy,
}

/// 一条不包含原始 TOML、credential 或底层错误文本的安全诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigIssue {
    pub(super) code: ConfigIssueCode,
    pub(super) message: &'static str,
}

impl ConfigIssue {
    /// 稳定诊断分类。
    pub fn code(&self) -> ConfigIssueCode {
        self.code
    }

    /// 可安全展示的固定消息。
    pub fn message(&self) -> &'static str {
        self.message
    }
}

/// 当前 schema 支持的模型协议。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ModelProtocol {
    /// OpenAI Chat Completions 协议。
    OpenAiChatCompletions,
    /// OpenAI Responses 协议。
    OpenAiResponses,
}

impl ModelProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiChatCompletions => "openai_chat_completions",
            Self::OpenAiResponses => "openai_responses",
        }
    }
}

/// 已校验的模型传输超时配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeModelTransportConfig {
    pub(super) connect_timeout: Duration,
    pub(super) request_timeout: Duration,
}

/// 已校验的 MCP 进程级运行参数，包含调用缺省时限及连接、目录、关闭和并发上限。
///
/// 这里只保存连接和调用策略，不包含任何 Server 配置、目录或 credential。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpRuntimeConfig {
    pub(super) connect_timeout: Duration,
    pub(super) catalog_timeout: Duration,
    pub(super) request_timeout: Duration,
    pub(super) close_timeout: Duration,
    pub(super) max_concurrent_calls_per_server: std::num::NonZeroU32,
}

impl McpRuntimeConfig {
    /// 毫秒值需能通过 Desktop 的 JSON number 无损往返；这是数值边界，不是业务耗时上限。
    pub(crate) const MAX_TOOL_TIMEOUT_INTEGER_MS: u64 = (1_u64 << 53) - 1;

    pub fn connect_timeout(self) -> Duration {
        self.connect_timeout
    }

    pub fn catalog_timeout(self) -> Duration {
        self.catalog_timeout
    }

    /// 未配置 Server `toolTimeoutMs` 时使用；不是 Server 调用时限的上限。
    pub fn request_timeout(self) -> Duration {
        self.request_timeout
    }

    pub fn close_timeout(self) -> Duration {
        self.close_timeout
    }

    pub fn max_concurrent_calls_per_server(self) -> std::num::NonZeroU32 {
        self.max_concurrent_calls_per_server
    }

    #[cfg(test)]
    pub(crate) fn test_default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(1),
            catalog_timeout: Duration::from_secs(1),
            request_timeout: Duration::from_secs(1),
            close_timeout: Duration::from_secs(1),
            max_concurrent_calls_per_server: std::num::NonZeroU32::new(2)
                .expect("test MCP concurrency is non-zero"),
        }
    }
}

/// 已校验的单层子任务委派上限；所有字段都显式大于零。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelegationConfig {
    pub(super) max_tasks_per_run: std::num::NonZeroU32,
    pub(super) max_concurrent_tasks: std::num::NonZeroU32,
    pub(super) task_timeout: Duration,
    pub(super) max_steps: std::num::NonZeroU32,
    pub(super) max_tool_calls: std::num::NonZeroU32,
    pub(super) max_output_tokens: std::num::NonZeroU32,
}

impl DelegationConfig {
    pub fn max_tasks_per_run(self) -> std::num::NonZeroU32 {
        self.max_tasks_per_run
    }

    pub fn max_concurrent_tasks(self) -> std::num::NonZeroU32 {
        self.max_concurrent_tasks
    }

    pub fn task_timeout(self) -> Duration {
        self.task_timeout
    }

    pub fn max_steps(self) -> std::num::NonZeroU32 {
        self.max_steps
    }

    pub fn max_tool_calls(self) -> std::num::NonZeroU32 {
        self.max_tool_calls
    }

    pub fn max_output_tokens(self) -> std::num::NonZeroU32 {
        self.max_output_tokens
    }
}

impl RuntimeModelTransportConfig {
    /// 建立连接的最长等待时间。
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// 等待响应建立及相邻流 chunk 的最长时间；不限制流式响应总时长。
    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

/// 进程内最小 secret wrapper。
///
/// 本类型刻意不实现 `Serialize`，也不公开内部字符串字段；Debug 永远脱敏。当前版本不承诺
/// 内存硬件级清零，但通过较窄的构造与读取边界避免无意义复制和观察输出泄漏。
#[derive(Clone)]
pub(super) struct ModelSecret(String);

impl ModelSecret {
    /// 从已经完成本地格式校验的原值构造；调用方不能在此处 trim 或规范化 credential。
    pub(super) fn new(value: String) -> Self {
        Self(value)
    }

    /// 只供 ResolvedModelConfig 的显式 ModelService 构造入口读取。
    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ModelSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// 一条有效模型配置及其已编译的请求参数。
///
/// 该类型不实现 Debug/Serialize；只有明确的只读 getter 可跨越配置编译边界。它同时保留
/// 模型静态硬上限和已计算的 generation，使后续 Run 不必重新解释配置覆盖规则。
pub struct ResolvedModelConfig {
    pub(super) display_name: String,
    pub(super) protocol: ModelProtocol,
    pub(super) provider: ProviderId,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) api_key: ModelSecret,
    pub(super) context_window_tokens: u64,
    pub(super) max_input_tokens: Option<u64>,
    pub(super) max_output_tokens: u32,
    pub(super) generation: GenerationConfig,
    pub(super) capabilities: super::capabilities::ResolvedModelCapabilities,
}

impl ResolvedModelConfig {
    /// 用户可见名称。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 已解析协议。
    pub fn protocol(&self) -> ModelProtocol {
        self.protocol
    }

    /// 用户配置的供应商标识；它不是协议或方言名称。
    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    /// 已通过安全规则校验的 endpoint 原值。
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Provider 模型名原值。
    pub fn model(&self) -> &str {
        &self.model
    }

    /// 仅供后续 ModelService 构造边界读取 credential；不得写入日志或协议投影。
    pub fn api_key(&self) -> &str {
        self.api_key.expose()
    }

    /// 当前编译模型的上下文窗口上限。
    pub fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    /// 当前思考模式的独立输入上限；未知时不从上下文窗口猜测一个值。
    pub fn max_input_tokens(&self) -> Option<u64> {
        self.max_input_tokens
    }

    /// 模型声明的单轮最大输出上限。
    pub fn max_output_tokens(&self) -> u32 {
        self.max_output_tokens
    }

    /// 已将 Agent 请求上限与模型硬上限取最小值的 generation 配置。
    pub fn generation(&self) -> &GenerationConfig {
        &self.generation
    }

    /// 静态目录、用户 override 与协议基线合并后的唯一能力事实。
    pub fn capabilities(&self) -> &super::capabilities::ResolvedModelCapabilities {
        &self.capabilities
    }
}

/// 已通过顶层与全局校验的配置快照。
///
/// 文件缺失使用明确的全局缺省值；无效文件不回退旧快照。模型是否可用由数据库模型准备判断。
pub struct ResolvedConfig {
    pub(super) schema_version: u32,
    pub(super) transport: RuntimeModelTransportConfig,
    pub(super) generation: GenerationConfig,
    pub(super) retry_policy: Option<ModelRetryPolicy>,
    pub(super) budget: ExecutionBudget,
    pub(super) guardrails: GuardrailConfig,
    pub(super) delegation: DelegationConfig,
    pub(super) mcp: McpRuntimeConfig,
    pub(super) vision: Option<VisionConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisionConfig {
    pub timeout: Duration,
    pub max_output_tokens: u32,
}

impl ResolvedConfig {
    /// 当前配置 schema version。
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 全局模型传输配置。
    pub fn transport(&self) -> RuntimeModelTransportConfig {
        self.transport
    }

    /// 模型无关的请求预算；执行准备时才与所选模型的生效模式限制组合。
    pub fn generation(&self) -> &GenerationConfig {
        &self.generation
    }

    /// 显式有限重试策略；None 表示不启用隐藏重试。
    pub fn retry_policy(&self) -> Option<&ModelRetryPolicy> {
        self.retry_policy.as_ref()
    }

    /// Agent 全局执行预算；未配置字段保持 None。
    pub fn budget(&self) -> &ExecutionBudget {
        &self.budget
    }

    /// Agent 全局 Guardrail；每个 Run 从配置快照冻结一次。
    pub fn guardrails(&self) -> &GuardrailConfig {
        &self.guardrails
    }

    /// 单层子任务委派的模型无关调度与执行上限。
    pub fn delegation(&self) -> DelegationConfig {
        self.delegation
    }

    /// MCP 进程级运行参数。实际 MCP 能力尚未装配时该配置也可以安全存在。
    pub fn mcp(&self) -> McpRuntimeConfig {
        self.mcp
    }

    pub fn vision(&self) -> Option<&VisionConfig> {
        self.vision.as_ref()
    }
}

/// 配置状态查询可使用的整体脱敏投影。
///
/// 这是未来协议 DTO 的内部来源，但本类型本身不直接 Serialize，以便协议版本独立演进并再次
/// 审核允许跨层展示的字段。
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigProjection {
    /// 当前状态。
    pub state: ConfigState,
    /// 成功读取到的 schema version。
    pub schema_version: Option<u32>,
    /// 已成功编译的委派上限；全局配置无效或缺失时不存在。
    pub delegation: Option<DelegationConfig>,
    /// 全部安全诊断。
    pub issues: Vec<ConfigIssue>,
}

/// 一次纯配置编译的完整结果。
///
/// `active` 与 `projection` 同时返回，调用方无需从展示数据反向构造执行配置。状态与 active
/// 无效文件没有 active；文件缺失使用产品全局缺省值，仍通过 Missing 表示文件状态。
pub struct ConfigCompilation {
    pub(super) state: ConfigState,
    pub(super) active: Option<ResolvedConfig>,
    pub(super) projection: ConfigProjection,
}

impl ConfigCompilation {
    /// 构造“配置文件不存在”的结果，供后续配置源复用。
    ///
    /// 文件缺失不是 TOML 错误，也不产生伪造 issue；Runtime 可以正常启动并通过状态查询提示。
    pub fn missing() -> Self {
        // 无文件使用产品已声明的全局缺省值；缺少模型由独立模型设置诊断。
        let mut compilation = super::compile_runtime_config("schema_version = 1\n");
        compilation.state = ConfigState::Missing;
        compilation.projection.state = ConfigState::Missing;
        compilation.projection.schema_version = None;
        compilation
    }

    /// 构造“配置源存在但无法安全交付文档”的 fail-closed 结果。
    pub(super) fn source_unavailable(kind: ConfigSourceFailureKind, message: &'static str) -> Self {
        let code = match kind {
            ConfigSourceFailureKind::Unsafe => ConfigIssueCode::UnsafeConfigSource,
            ConfigSourceFailureKind::Read => ConfigIssueCode::ConfigReadFailed,
        };
        let issue = ConfigIssue { code, message };
        Self {
            state: ConfigState::Invalid,
            active: None,
            projection: ConfigProjection {
                state: ConfigState::Invalid,
                schema_version: None,
                delegation: None,
                issues: vec![issue],
            },
        }
    }

    /// 当前配置状态。
    pub fn state(&self) -> ConfigState {
        self.state
    }

    /// 已形成的有效快照；Invalid 时为 None。
    pub fn active(&self) -> Option<&ResolvedConfig> {
        self.active.as_ref()
    }

    /// 不含 credential 的查询投影。
    pub fn projection(&self) -> &ConfigProjection {
        &self.projection
    }

    /// 全部安全诊断。
    pub fn issues(&self) -> &[ConfigIssue] {
        &self.projection.issues
    }
}
