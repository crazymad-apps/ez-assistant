//! 配置编译产生的领域状态、有效快照和脱敏投影。
//!
//! 同一份用户配置在 Runtime 内有两个严格分离的视图：`ResolvedConfig` 服务于执行装配，
//! 可以持有 secret；`ConfigProjection` 服务于查询和未来应用协议，只能包含脱敏事实。
//! 两者不通过通用 Serialize 相互转换，避免新增字段时意外把 credential 带出进程边界。

use std::{collections::BTreeMap, fmt, time::Duration};

use agent_core::{ExecutionBudget, GuardrailConfig};
use agent_model::{GenerationConfig, ModelRetryPolicy};
use agent_types::ProviderId;
use assistant_protocol::ModelKey;

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
    /// 模型 key 不符合约定形式。
    InvalidModelKey,
    /// 单个模型表无法解释或字段值无效。
    InvalidModel,
    /// 协议不受当前 schema 支持。
    UnsupportedProtocol,
    /// Provider 标识不符合当前 schema 的稳定 key 形式。
    InvalidProvider,
    /// endpoint 不满足安全 URL 约束。
    InvalidEndpoint,
    /// API Key 缺失或格式无效。
    MissingCredential,
    /// token、超时或执行上限无效。
    InvalidLimit,
    /// Runtime/Agent 策略无效。
    InvalidPolicy,
    /// 模型协议 Adapter 与 Agent 全局请求参数不兼容。
    UnsupportedProfileCombination,
    /// 默认 key 不存在或指向无效模型。
    DefaultModelUnavailable,
}

/// 一条不包含原始 TOML、credential 或底层错误文本的安全诊断。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigIssue {
    pub(super) code: ConfigIssueCode,
    pub(super) model_key: Option<ModelKey>,
    pub(super) message: &'static str,
}

impl ConfigIssue {
    /// 稳定诊断分类。
    pub fn code(&self) -> ConfigIssueCode {
        self.code
    }

    /// 诊断所属的合法模型 key；全局或非法 key 诊断为 None。
    pub fn model_key(&self) -> Option<&ModelKey> {
        self.model_key.as_ref()
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

    pub(crate) fn parse_config(value: &str) -> Option<Self> {
        match value {
            "openai_chat_completions" | "chat_completions" => Some(Self::OpenAiChatCompletions),
            "openai_responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }

    pub(super) fn parse_catalog(value: &str) -> Option<Self> {
        match value {
            "openai_chat_completions" => Some(Self::OpenAiChatCompletions),
            "openai_responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }
}

/// 已校验的模型传输超时配置。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeModelTransportConfig {
    pub(super) connect_timeout: Duration,
    pub(super) request_timeout: Duration,
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
    pub(super) key: ModelKey,
    pub(super) display_name: String,
    pub(super) protocol: ModelProtocol,
    pub(super) provider: ProviderId,
    pub(super) endpoint: String,
    pub(super) model: String,
    pub(super) api_key: ModelSecret,
    pub(super) context_window_tokens: u64,
    pub(super) max_output_tokens: u32,
    pub(super) generation: GenerationConfig,
    pub(super) capabilities: super::catalog::ResolvedModelCapabilities,
}

impl ResolvedModelConfig {
    /// 用户配置的稳定 key。
    pub fn key(&self) -> &ModelKey {
        &self.key
    }

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

    /// 模型静态上下文窗口上限。
    pub fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
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
    pub fn capabilities(&self) -> &super::catalog::ResolvedModelCapabilities {
        &self.capabilities
    }
}

/// 已通过顶层与全局校验的配置快照。
///
/// Degraded 状态也会拥有本类型：models 只收录有效条目，default_model 可以合法但暂时不可用。
/// Missing/Invalid 则完全没有快照，避免回退到旧 credential 或半解析的全局策略。
pub struct ResolvedConfig {
    pub(super) schema_version: u32,
    pub(super) default_model: Option<ModelKey>,
    pub(super) transport: RuntimeModelTransportConfig,
    pub(super) retry_policy: Option<ModelRetryPolicy>,
    pub(super) budget: ExecutionBudget,
    pub(super) guardrails: GuardrailConfig,
    pub(super) delegation: DelegationConfig,
    pub(super) vision: Option<VisionConfig>,
    pub(super) models: BTreeMap<ModelKey, ResolvedModelConfig>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisionConfig {
    pub model_key: ModelKey,
    pub timeout: Duration,
    pub max_output_tokens: u32,
}

impl ResolvedConfig {
    /// 当前配置 schema version。
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// 已通过 key 形式校验的默认模型；它仍可能不在有效模型 map 中。
    pub fn default_model(&self) -> Option<&ModelKey> {
        self.default_model.as_ref()
    }

    /// 全局模型传输配置。
    pub fn transport(&self) -> RuntimeModelTransportConfig {
        self.transport
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

    pub fn vision(&self) -> Option<&VisionConfig> {
        self.vision.as_ref()
    }

    /// 按 key 确定性排序的有效模型集合。
    pub fn models(&self) -> &BTreeMap<ModelKey, ResolvedModelConfig> {
        &self.models
    }

    /// 查询一条有效模型配置。
    pub fn model(&self, key: &ModelKey) -> Option<&ResolvedModelConfig> {
        self.models.get(key)
    }
}

/// 单个模型的脱敏配置投影。
///
/// 投影允许表达无效模型，因此大多数字段是 Option；`api_key_configured` 只说明本地格式通过，
/// 不说明凭证已经联网验证。任何情况下都不能从本类型还原 API Key。
#[derive(Clone, Debug, PartialEq)]
pub struct ModelConfigProjection {
    /// 合法模型 key；非法 key 不回显原始表名。
    pub model_key: Option<ModelKey>,
    /// 显示名称；无效 key 且无显式名称时使用固定占位符。
    pub display_name: String,
    /// 原始安全协议名称。
    pub protocol: Option<String>,
    /// 原始安全 Provider 名称。
    pub provider: Option<String>,
    /// 只有通过 userinfo/query/fragment 校验后才投影。
    pub endpoint: Option<String>,
    /// Provider 模型名。
    pub model: Option<String>,
    /// 模型上下文窗口上限。
    pub context_window_tokens: Option<u64>,
    /// 模型单轮最大输出上限。
    pub max_output_tokens: Option<u32>,
    /// Agent 全局请求的最大输出值。
    pub agent_max_output_tokens: Option<u32>,
    /// 两个输出维度取最小值后的结果。
    pub effective_max_output_tokens: Option<u32>,
    /// 已编译能力是否支持原生图片输入；无效模型保持 false。
    pub supports_image_input: bool,
    /// 只表示 API Key 已通过本地非空格式校验，不包含其值。
    pub api_key_configured: bool,
    /// 是否对应配置中的默认 key。
    pub is_default: bool,
    /// 是否已进入有效模型 map。
    pub is_valid: bool,
    /// 本模型的安全诊断。
    pub issues: Vec<ConfigIssue>,
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
    /// 合法的默认模型 key。
    pub default_model: Option<ModelKey>,
    /// 形式合法的辅助视觉模型 key；目标模型无效时仍保留以便诊断和修复。
    pub auxiliary_vision_model: Option<ModelKey>,
    /// 已成功编译的委派上限；全局配置无效或缺失时不存在。
    pub delegation: Option<DelegationConfig>,
    /// 按配置 key 确定性排序的模型投影。
    pub models: Vec<ModelConfigProjection>,
    /// 全部安全诊断。
    pub issues: Vec<ConfigIssue>,
}

/// 一次纯配置编译的完整结果。
///
/// `active` 与 `projection` 同时返回，调用方无需从展示数据反向构造执行配置。状态与 active
/// 的不变量是：Missing/Invalid 为 None，Degraded/Ready 为 Some。
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
        Self {
            state: ConfigState::Missing,
            active: None,
            projection: ConfigProjection {
                state: ConfigState::Missing,
                schema_version: None,
                default_model: None,
                auxiliary_vision_model: None,
                delegation: None,
                models: Vec::new(),
                issues: Vec::new(),
            },
        }
    }

    /// 构造“配置源存在但无法安全交付文档”的 fail-closed 结果。
    pub(super) fn source_unavailable(kind: ConfigSourceFailureKind, message: &'static str) -> Self {
        let code = match kind {
            ConfigSourceFailureKind::Unsafe => ConfigIssueCode::UnsafeConfigSource,
            ConfigSourceFailureKind::Read => ConfigIssueCode::ConfigReadFailed,
        };
        let issue = ConfigIssue {
            code,
            model_key: None,
            message,
        };
        Self {
            state: ConfigState::Invalid,
            active: None,
            projection: ConfigProjection {
                state: ConfigState::Invalid,
                schema_version: None,
                default_model: None,
                auxiliary_vision_model: None,
                delegation: None,
                models: Vec::new(),
                issues: vec![issue],
            },
        }
    }

    /// 当前配置状态。
    pub fn state(&self) -> ConfigState {
        self.state
    }

    /// 已形成的有效快照；Missing/Invalid 时为 None。
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
