//! 顶层配置解析、全局策略编译和整体状态决策。
//!
//! 本模块只决定“整份配置是否能形成快照”以及 Runtime/Agent 全局策略。模型表先作为
//! TOML value map 留在 [`RawConfig`] 中，再交给 `model` 模块逐条编译；这样全局错误可以
//! fail-closed，而单模型错误只让配置进入 Degraded，不会拖垮其他有效模型。

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
    time::Duration,
};

use agent_core::{ActiveGuardrailMode, ExecutionBudget, GuardrailCheckConfig, GuardrailConfig};
use agent_model::{GenerationConfig, ModelRetryPolicy, ModelRetryReason};
use assistant_protocol::ModelKey;

use super::{
    catalog::ModelCatalog,
    domain::{
        ConfigCompilation, ConfigIssue, ConfigIssueCode, ConfigProjection, ConfigState,
        ResolvedConfig, RuntimeModelTransportConfig,
    },
    model::compile_model,
    schema::{
        RawConfig, RawDelegationConfig, RawExecutionLimits, RawGenerationConfig, RawGuardrailCheck,
        RawGuardrailConfig, RawGuardrailMode, RawModelRetryConfig, RawRuntimeConfig,
    },
};

/// 当前编译器唯一理解的持久化 schema；不对未知版本做猜测兼容。
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// 解析并编译一份 schema version 1 的 config.toml 文本。
///
/// 编译分为四层：TOML 语法、顶层 schema、全局策略、逐模型语义。前三层失败时没有
/// active 快照；逐模型错误则保留有效模型和安全诊断。整个过程是纯函数，不读取文件、
/// 不修改 Runtime registry，也不向错误中附带原始 TOML。
pub fn compile_runtime_config(document: &str) -> ConfigCompilation {
    compile_runtime_config_with_catalog(document, &ModelCatalog::empty())
}

/// 使用已校验的随包目录编译 config.toml；目录本身不由该纯函数读取或写回。
pub fn compile_runtime_config_with_catalog(
    document: &str,
    catalog: &ModelCatalog,
) -> ConfigCompilation {
    // 先用无业务类型的 Value 检查语法和重复 key。若直接进入 serde 结构，语法错误和
    // 顶层类型错误会混在一起，也更容易误把包含源码片段的底层错误向上透出。
    if toml::from_str::<toml::Value>(document).is_err() {
        return invalid_compilation(
            None,
            None,
            ConfigIssueCode::InvalidSyntax,
            "configuration is not valid TOML",
        );
    }

    // 第二遍只解析全局结构；models 的 value 仍未进入 RawModelConfig。
    let raw = match toml::from_str::<RawConfig>(document) {
        Ok(raw) => raw,
        Err(error) => {
            let code = classify_deserialization_error(&error, ConfigIssueCode::InvalidTopLevel);
            return invalid_compilation(
                None,
                None,
                code,
                "top-level configuration structure is invalid",
            );
        }
    };

    // 未知版本不能沿用当前默认值和字段含义，否则可能形成表面成功但语义错误的配置。
    if raw.schema_version != SUPPORTED_SCHEMA_VERSION {
        return invalid_compilation(
            Some(raw.schema_version),
            None,
            ConfigIssueCode::UnsupportedSchemaVersion,
            "configuration schema version is not supported",
        );
    }

    // 全局策略会影响每一个新 Run，因此任何一个全局错误都必须让整份配置不可用。
    let global = match compile_global(
        &raw.runtime,
        &raw.agent.defaults.generation,
        &raw.agent.defaults.execution_limits,
        &raw.agent.defaults.guardrails,
        &raw.agent.defaults.delegation,
        raw.agent.vision.as_ref(),
    ) {
        Ok(global) => global,
        Err(issues) => {
            return ConfigCompilation {
                state: ConfigState::Invalid,
                active: None,
                projection: ConfigProjection {
                    state: ConfigState::Invalid,
                    schema_version: Some(raw.schema_version),
                    default_model: ModelKey::new(raw.default_model).ok(),
                    auxiliary_vision_model: None,
                    delegation: None,
                    models: Vec::new(),
                    issues,
                },
            };
        }
    };

    // default_model 形式无效不妨碍用户显式选择其他有效模型，所以属于 Degraded 而非 Invalid。
    let mut all_issues = Vec::new();
    let default_model = match ModelKey::new(raw.default_model.clone()) {
        Ok(key) => Some(key),
        Err(_) => {
            all_issues.push(global_issue(
                ConfigIssueCode::InvalidModelKey,
                "default model key is invalid",
            ));
            None
        }
    };

    // BTreeMap 同时保证模型查找使用强类型 key，并为后续协议投影提供确定性顺序。
    let mut valid_models = BTreeMap::new();
    let mut model_projections = Vec::with_capacity(raw.models.len());
    for (raw_key, value) in raw.models {
        let output = compile_model(
            raw_key,
            value,
            &raw.default_model,
            &global.generation,
            catalog,
        );
        all_issues.extend(output.projection.issues.iter().cloned());
        if let Some(model) = output.resolved {
            valid_models.insert(model.key().clone(), model);
        }
        model_projections.push(output.projection);
    }

    // 默认 key 既要形式合法，也必须确实进入有效模型 map；仅在原始 models 表中出现不够。
    let default_is_available = default_model
        .as_ref()
        .is_some_and(|key| valid_models.contains_key(key));
    if !default_is_available {
        all_issues.push(ConfigIssue {
            code: ConfigIssueCode::DefaultModelUnavailable,
            model_key: default_model.clone(),
            message: "default model is not available",
        });
    }

    let auxiliary_vision_model = global
        .vision
        .as_ref()
        .map(|vision| vision.model_key.clone());
    let vision = global.vision.filter(|vision| {
        let valid = valid_models
            .get(&vision.model_key)
            .is_some_and(|model| model.capabilities().image_input);
        if !valid {
            all_issues.push(ConfigIssue {
                code: ConfigIssueCode::InvalidModel,
                model_key: None,
                message: "auxiliary vision model is unavailable or lacks image input",
            });
        }
        valid
    });
    let state = if all_issues.is_empty() {
        ConfigState::Ready
    } else {
        ConfigState::Degraded
    };
    let resolved = ResolvedConfig {
        schema_version: raw.schema_version,
        default_model: default_model.clone(),
        transport: global.transport,
        retry_policy: global.retry_policy,
        budget: global.budget,
        guardrails: global.guardrails,
        delegation: global.delegation,
        vision,
        models: valid_models,
    };
    ConfigCompilation {
        state,
        active: Some(resolved),
        projection: ConfigProjection {
            state,
            schema_version: Some(raw.schema_version),
            default_model,
            auxiliary_vision_model,
            delegation: Some(global.delegation),
            models: model_projections,
            issues: all_issues,
        },
    }
}

/// 已映射到现有下层契约的全局配置中间结果。
///
/// generation 在单模型编译时还要与模型输出硬上限合并，因此此处暂不作为最终请求配置。
struct CompiledGlobalConfig {
    /// Runtime 统一控制的连接、响应建立和流空闲超时。
    transport: RuntimeModelTransportConfig,
    /// 显式配置才存在的有限建流前重试策略。
    retry_policy: Option<ModelRetryPolicy>,
    /// Agent 请求维度的 generation 默认值。
    generation: GenerationConfig,
    /// 与模型上下文窗口、模型输出硬上限相互独立的执行预算。
    budget: ExecutionBudget,
    /// 已映射到 Core 的显式 Guardrail 配置。
    guardrails: GuardrailConfig,
    /// 单层子任务委派的显式产品上限。
    delegation: super::domain::DelegationConfig,
    vision: Option<super::domain::VisionConfig>,
}

/// 校验 Runtime/Agent 全局字段并直接映射到已有领域类型。
///
/// 这里不读取 Provider 协议 Adapter，也不处理 tool choice/reasoning；这些是 Run 业务编译职责，
/// 不是用户静态配置维度。
fn compile_global(
    runtime: &RawRuntimeConfig,
    generation: &RawGenerationConfig,
    limits: &RawExecutionLimits,
    guardrails: &RawGuardrailConfig,
    delegation: &RawDelegationConfig,
    vision: Option<&super::schema::RawVisionConfig>,
) -> Result<CompiledGlobalConfig, Vec<ConfigIssue>> {
    let mut issues = Vec::new();
    // request timeout 同时约束等待响应建立；必须覆盖 connect timeout，避免连接阶段
    // 拥有更长的预算。流建立后该值改为相邻 chunk 的空闲上限，不限制流的总时长。
    let transport = &runtime.model_transport;
    if transport.connect_timeout_ms == 0
        || transport.request_timeout_ms == 0
        || transport.request_timeout_ms < transport.connect_timeout_ms
    {
        issues.push(global_issue(
            ConfigIssueCode::InvalidLimit,
            "model transport timeouts are invalid",
        ));
    }

    // 下层 GenerationConfig 有意不做范围判断；用户配置的输入校验由 Runtime 统一承担。
    if generation
        .temperature
        .is_some_and(|value| !value.is_finite() || value < 0.0)
        || generation
            .top_p
            .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > 1.0)
        || generation.max_output_tokens == Some(0)
        || generation.stop.iter().any(|stop| stop.trim().is_empty())
    {
        issues.push(global_issue(
            ConfigIssueCode::InvalidPolicy,
            "agent generation defaults are invalid",
        ));
    }

    // None 代表不注入隐藏限制；显式的 0 不等于“关闭”，而是无意义配置。
    if limits.max_steps == Some(0) || limits.max_tool_calls == Some(0) {
        issues.push(global_issue(
            ConfigIssueCode::InvalidLimit,
            "agent execution limits must be positive",
        ));
    }

    if guardrails.repeated_invocation.threshold == 0
        || guardrails.consecutive_failures.threshold == 0
    {
        issues.push(global_issue(
            ConfigIssueCode::InvalidLimit,
            "agent guardrail thresholds must be positive",
        ));
    }

    if delegation.max_tasks_per_run == 0
        || delegation.max_concurrent_tasks == 0
        || delegation.task_timeout_ms == 0
        || delegation.max_steps == 0
        || delegation.max_tool_calls == 0
        || delegation.max_output_tokens == 0
        || delegation.max_concurrent_tasks > delegation.max_tasks_per_run
    {
        issues.push(global_issue(
            ConfigIssueCode::InvalidLimit,
            "agent delegation limits are invalid",
        ));
    }

    let vision = match vision {
        Some(vision)
            if vision.timeout_ms > 0
                && vision.max_output_tokens > 0
                && ModelKey::new(vision.model_key.clone()).is_ok() =>
        {
            Some(super::domain::VisionConfig {
                model_key: ModelKey::new(vision.model_key.clone())
                    .expect("vision model key was validated"),
                timeout: Duration::from_millis(vision.timeout_ms),
                max_output_tokens: vision.max_output_tokens,
            })
        }
        Some(_) => {
            issues.push(global_issue(
                ConfigIssueCode::InvalidLimit,
                "auxiliary vision configuration is invalid",
            ));
            None
        }
        None => None,
    };

    let retry_policy = match runtime.model_retry.as_ref() {
        Some(retry) => compile_retry_policy(retry, &mut issues),
        None => None,
    };
    if !issues.is_empty() {
        return Err(issues);
    }

    Ok(CompiledGlobalConfig {
        transport: RuntimeModelTransportConfig {
            connect_timeout: Duration::from_millis(transport.connect_timeout_ms),
            request_timeout: Duration::from_millis(transport.request_timeout_ms),
        },
        retry_policy,
        generation: GenerationConfig {
            temperature: generation.temperature,
            top_p: generation.top_p,
            max_output_tokens: generation.max_output_tokens,
            stop: generation.stop.clone(),
        },
        budget: ExecutionBudget {
            max_steps: limits.max_steps,
            max_tool_calls: limits.max_tool_calls,
        },
        guardrails: GuardrailConfig {
            repeated_invocation: compile_guardrail_check(&guardrails.repeated_invocation),
            consecutive_failures: compile_guardrail_check(&guardrails.consecutive_failures),
        },
        delegation: super::domain::DelegationConfig {
            max_tasks_per_run: NonZeroU32::new(delegation.max_tasks_per_run)
                .expect("delegation task limit was validated"),
            max_concurrent_tasks: NonZeroU32::new(delegation.max_concurrent_tasks)
                .expect("delegation concurrency was validated"),
            task_timeout: Duration::from_millis(delegation.task_timeout_ms),
            max_steps: NonZeroU32::new(delegation.max_steps)
                .expect("delegation step limit was validated"),
            max_tool_calls: NonZeroU32::new(delegation.max_tool_calls)
                .expect("delegation tool limit was validated"),
            max_output_tokens: NonZeroU32::new(delegation.max_output_tokens)
                .expect("delegation output limit was validated"),
        },
        vision,
    })
}

fn compile_guardrail_check(raw: &RawGuardrailCheck) -> Option<GuardrailCheckConfig> {
    let mode = match raw.mode {
        RawGuardrailMode::Off => return None,
        RawGuardrailMode::Observe => ActiveGuardrailMode::Observe,
        RawGuardrailMode::Enforce => ActiveGuardrailMode::Enforce,
    };
    Some(GuardrailCheckConfig {
        mode,
        threshold: NonZeroU32::new(raw.threshold)
            .expect("guardrail threshold was validated before compilation"),
    })
}

/// 将用户字符串枚举编译成 provider-neutral 的有限重试策略。
///
/// 整个 retry 表缺失表示不重试；表一旦出现，就必须完整声明原因、延迟序列和
/// Retry-After 上限，避免启用半套隐式策略。
fn compile_retry_policy(
    raw: &RawModelRetryConfig,
    issues: &mut Vec<ConfigIssue>,
) -> Option<ModelRetryPolicy> {
    let Some(retry_on) = raw.retry_on.as_ref().filter(|values| !values.is_empty()) else {
        issues.push(global_issue(
            ConfigIssueCode::InvalidPolicy,
            "model retry reasons must be present and non-empty",
        ));
        return None;
    };
    let Some(delays_ms) = raw.delays_ms.as_ref().filter(|values| !values.is_empty()) else {
        issues.push(global_issue(
            ConfigIssueCode::InvalidPolicy,
            "model retry delays must be present and non-empty",
        ));
        return None;
    };
    let Some(max_retry_after_ms) = raw.max_retry_after_ms else {
        issues.push(global_issue(
            ConfigIssueCode::InvalidPolicy,
            "maximum retry-after must be present",
        ));
        return None;
    };

    // BTreeSet 与 ModelRetryPolicy 的事实类型一致；重复原因没有额外语义，确定性去重即可。
    let mut reasons = BTreeSet::new();
    for value in retry_on {
        let reason = match value.as_str() {
            "connection" => ModelRetryReason::Connection,
            "timeout" => ModelRetryReason::Timeout,
            "rate_limited" => ModelRetryReason::RateLimited,
            "unavailable" => ModelRetryReason::Unavailable,
            _ => {
                issues.push(global_issue(
                    ConfigIssueCode::InvalidPolicy,
                    "model retry contains an unsupported reason",
                ));
                return None;
            }
        };
        reasons.insert(reason);
    }

    Some(ModelRetryPolicy::new(
        reasons,
        delays_ms
            .iter()
            .copied()
            .map(Duration::from_millis)
            .collect(),
        Duration::from_millis(max_retry_after_ms),
    ))
}

/// 将 serde/TOML 的结构错误压缩为安全、稳定的内部分类。
///
/// 底层错误只在当前栈帧中用于分类，不保存、不回显；不能识别时回退到调用方提供的
/// 顶层或单模型分类，避免把原始 TOML 片段带入 issue。
pub(super) fn classify_deserialization_error(
    error: &toml::de::Error,
    fallback: ConfigIssueCode,
) -> ConfigIssueCode {
    let description = error.to_string();
    if description.contains("unknown field") {
        ConfigIssueCode::UnknownField
    } else if description.contains("missing field") {
        ConfigIssueCode::MissingField
    } else {
        fallback
    }
}

/// 构造不归属于某个合法模型 key 的固定安全诊断。
pub(super) fn global_issue(code: ConfigIssueCode, message: &'static str) -> ConfigIssue {
    ConfigIssue {
        code,
        model_key: None,
        message,
    }
}

/// 构造没有 active 快照的 Invalid 结果。
///
/// 只允许传入已经脱敏的静态 message；调用方不得把底层解析错误文本传到这里。
fn invalid_compilation(
    schema_version: Option<u32>,
    default_model: Option<ModelKey>,
    code: ConfigIssueCode,
    message: &'static str,
) -> ConfigCompilation {
    let issue = global_issue(code, message);
    ConfigCompilation {
        state: ConfigState::Invalid,
        active: None,
        projection: ConfigProjection {
            state: ConfigState::Invalid,
            schema_version,
            default_model,
            auxiliary_vision_model: None,
            delegation: None,
            models: Vec::new(),
            issues: vec![issue],
        },
    }
}
