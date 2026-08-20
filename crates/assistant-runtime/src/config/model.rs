//! 单模型配置的语义编译、协议 Adapter 组合和安全投影。
//!
//! 本模块是顶层 fail-closed 之后的隔离边界：每个 `models.<key>` 独立产生有效模型或诊断，
//! 不会因为一个模型的未知字段、credential 或协议 Adapter 冲突而丢弃其他模型。

use agent_model::GenerationConfig;
use agent_types::ProviderId;
use assistant_protocol::ModelKey;
use url::Url;

use super::{
    catalog::{ModelCatalog, compile_capabilities},
    compile::{classify_deserialization_error, global_issue},
    domain::{
        ConfigIssue, ConfigIssueCode, ModelConfigProjection, ModelProtocol, ModelSecret,
        ResolvedModelConfig,
    },
    schema::RawModelConfig,
};

/// 单个模型的一次编译结果。
///
/// projection 始终存在，便于诊断无效条目；resolved 只有在全部模型语义通过时存在。
pub(super) struct ModelCompileOutput {
    /// 可被后续 Run 装配使用的完整模型配置，内部包含 secret。
    pub(super) resolved: Option<ResolvedModelConfig>,
    /// 永不包含 API Key 或不安全 endpoint 的展示投影。
    pub(super) projection: ModelConfigProjection,
}

/// 独立编译一个 `models.<key>` 条目，并尽量一次收集全部可操作诊断。
///
/// 函数不会联网，也不构造具体 Provider Adapter。协议 Adapter 只用于判断当前 schema 中已经确认的
/// 参数组合；Adapter 会在真正构造 ModelService 时再次验证自己的协议约束。
pub(super) fn compile_model(
    raw_key: String,
    value: toml::Value,
    raw_default_model: &str,
    agent_generation: &GenerationConfig,
    catalog: &ModelCatalog,
) -> ModelCompileOutput {
    // 先建立安全投影，即便后续 typed serde 失败，也能返回不含 secret 的有限诊断信息。
    // 非法原始 key 不回显，避免任意 TOML table name 进入日志或协议。
    let key = ModelKey::new(raw_key).ok();
    let mut projection =
        projection_from_value(&value, key.clone(), raw_default_model, agent_generation);
    if key.is_none() {
        projection.issues.push(global_issue(
            ConfigIssueCode::InvalidModelKey,
            "model key is invalid",
        ));
    }

    // 第二阶段 serde 只处理当前模型；未知字段或类型错误因此不会升级为顶层 Invalid。
    let raw = match value.try_into::<RawModelConfig>() {
        Ok(raw) => raw,
        Err(error) => {
            projection.issues.push(ConfigIssue {
                code: classify_deserialization_error(&error, ConfigIssueCode::InvalidModel),
                model_key: key,
                message: "model configuration structure is invalid",
            });
            return ModelCompileOutput {
                resolved: None,
                projection,
            };
        }
    };

    // protocol 决定 wire 契约；provider 保存实际供应商身份，并在 Runtime 内推导兼容协议 Adapter。
    let protocol = match raw.protocol.as_deref() {
        Some(value) if ModelProtocol::parse_config(value).is_some() => {
            let protocol = ModelProtocol::parse_config(value);
            projection.protocol = protocol.map(|protocol| protocol.as_str().to_owned());
            protocol
        }
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::UnsupportedProtocol,
                "model protocol is not supported",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "model protocol is required",
            ));
            None
        }
    };

    let provider = match raw.provider.as_deref() {
        Some(value) if validate_provider_id(value) => {
            projection.provider = Some(value.to_owned());
            ProviderId::new(value.to_owned()).ok()
        }
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidProvider,
                "model provider identifier is invalid",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "model provider is required",
            ));
            None
        }
    };

    // endpoint 只有通过安全 URL 规则后才允许写回 projection；含 userinfo/query/fragment 的
    // 原始值可能携带 token，不能为了诊断方便而回显。
    let endpoint = match raw.endpoint.as_deref() {
        Some(endpoint) if validate_endpoint(endpoint) => {
            projection.endpoint = Some(endpoint.to_owned());
            Some(endpoint.to_owned())
        }
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidEndpoint,
                "model endpoint is invalid",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "model endpoint is required",
            ));
            None
        }
    };

    let model = match raw.model {
        Some(model) if !model.trim().is_empty() => Some(model),
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidModel,
                "provider model name must not be blank",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "provider model name is required",
            ));
            None
        }
    };

    // API Key 只在 ResolvedModelConfig 的私有 wrapper 中保留。不同模型允许使用相同值，
    // 这里不建立 credential ID，也不进行跨模型相等性检查。
    let api_key = match raw.api_key {
        Some(api_key) if valid_api_key(&api_key) => {
            projection.api_key_configured = true;
            Some(ModelSecret::new(api_key))
        }
        Some(_) | None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingCredential,
                "model API key is missing or invalid",
            ));
            None
        }
    };

    // context window 是模型静态能力，不属于 Agent 执行预算。
    let context_window_tokens = match raw.context_window_tokens {
        Some(value) if value > 0 => Some(value),
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidLimit,
                "model context window must be positive",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "model context window is required",
            ));
            None
        }
    };

    // 此值是模型硬上限；Agent 请求上限稍后与它取最小值，但不会覆盖原始模型事实。
    let max_output_tokens = match raw.max_output_tokens {
        Some(value) if value > 0 => Some(value),
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidLimit,
                "model output limit must be positive",
            ));
            None
        }
        None => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::MissingField,
                "model output limit is required",
            ));
            None
        }
    };

    if context_window_tokens
        .zip(max_output_tokens)
        .is_some_and(|(context, output)| u64::from(output) > context)
    {
        projection.issues.push(model_issue(
            &key,
            ConfigIssueCode::InvalidLimit,
            "model output limit exceeds its context window",
        ));
    }

    let display_name = match raw.display_name {
        Some(display_name) if !display_name.trim().is_empty() => Some(display_name),
        Some(_) => {
            projection.issues.push(model_issue(
                &key,
                ConfigIssueCode::InvalidModel,
                "model display name must not be blank",
            ));
            None
        }
        None => key.as_ref().map(ToString::to_string),
    };

    let catalog_capabilities = provider
        .as_ref()
        .zip(protocol)
        .zip(model.as_deref())
        .map(|((provider, protocol), model)| catalog.resolve(provider, protocol, model));
    let capabilities = catalog_capabilities.as_ref().and_then(|base| {
        match compile_capabilities(&raw.capabilities, base) {
            Ok(capabilities) => Some(capabilities),
            Err(_) => {
                projection.issues.push(model_issue(
                    &key,
                    ConfigIssueCode::InvalidModel,
                    "model capability override is invalid",
                ));
                None
            }
        }
    });
    projection.supports_image_input = capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.image_input);

    // DeepSeek thinking 只有在精确目录或用户 override 声明 reasoning 时启用。
    if provider
        .as_ref()
        .is_some_and(|provider| provider.as_str() == "deepseek")
        && capabilities
            .as_ref()
            .is_some_and(|value| value.reasoning_enabled())
        && (agent_generation.temperature.is_some() || agent_generation.top_p.is_some())
    {
        projection.issues.push(model_issue(
            &key,
            ConfigIssueCode::UnsupportedProfileCombination,
            "DeepSeek thinking does not support configured temperature or top-p",
        ));
    }

    // 单轮请求上限有“模型硬上限”和“Agent 配置”两个来源，只在最终 generation 中取最小值；
    // context window 与 ExecutionBudget 均不参与这个公式。
    let effective_max_output_tokens = max_output_tokens.map(|model_limit| {
        agent_generation
            .max_output_tokens
            .map_or(model_limit, |agent_limit| model_limit.min(agent_limit))
    });
    projection.effective_max_output_tokens = effective_max_output_tokens;

    // projection 可以展示部分安全事实，但只要存在任一 issue，就不能把半有效配置交给 Run。
    if !projection.issues.is_empty() {
        return ModelCompileOutput {
            resolved: None,
            projection,
        };
    }

    // 不使用 expect 依赖前面的控制流不变量；若未来新增校验遗漏某个必填值，仍安全地保持无效。
    let resolved_parts = (
        key,
        display_name,
        protocol,
        provider,
        endpoint,
        model,
        api_key,
        context_window_tokens,
        max_output_tokens,
        effective_max_output_tokens,
        capabilities,
    );
    let resolved = match resolved_parts {
        (
            Some(key),
            Some(display_name),
            Some(protocol),
            Some(provider),
            Some(endpoint),
            Some(model),
            Some(api_key),
            Some(context_window_tokens),
            Some(max_output_tokens),
            Some(effective_max_output_tokens),
            Some(capabilities),
        ) => {
            let mut generation = agent_generation.clone();
            generation.max_output_tokens = Some(effective_max_output_tokens);
            Some(ResolvedModelConfig {
                key,
                display_name,
                protocol,
                provider,
                endpoint,
                model,
                api_key,
                context_window_tokens,
                max_output_tokens,
                generation,
                capabilities,
            })
        }
        _ => None,
    };
    projection.is_valid = resolved.is_some();
    ModelCompileOutput {
        resolved,
        projection,
    }
}

/// 在 typed model 解析前提取一份 best-effort 安全投影。
///
/// API Key 只转成 configured 布尔值；endpoint 初始永远为空，必须通过完整 URL 校验后由
/// compile_model 写入。字段类型不正确时不猜测或字符串化原始 TOML value。
fn projection_from_value(
    value: &toml::Value,
    key: Option<ModelKey>,
    raw_default_model: &str,
    agent_generation: &GenerationConfig,
) -> ModelConfigProjection {
    let table = value.as_table();
    let string_value = |field: &str| {
        table
            .and_then(|table| table.get(field))
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned)
    };
    let unsigned_value = |field: &str| {
        table
            .and_then(|table| table.get(field))
            .and_then(toml::Value::as_integer)
            .and_then(|value| u64::try_from(value).ok())
    };
    let display_name = string_value("display_name")
        .or_else(|| key.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| "<invalid model key>".to_owned());
    let context_window_tokens = unsigned_value("context_window_tokens");
    let max_output_tokens =
        unsigned_value("max_output_tokens").and_then(|value| u32::try_from(value).ok());

    ModelConfigProjection {
        is_default: key
            .as_ref()
            .is_some_and(|key| key.as_str() == raw_default_model),
        model_key: key,
        display_name,
        protocol: string_value("protocol"),
        // Provider 与 model key 一样属于标识；只有通过形式校验后才允许 compile_model 回填。
        provider: None,
        endpoint: None,
        model: string_value("model"),
        context_window_tokens,
        max_output_tokens,
        agent_max_output_tokens: agent_generation.max_output_tokens,
        effective_max_output_tokens: None,
        supports_image_input: false,
        api_key_configured: string_value("api_key").is_some_and(|value| valid_api_key(&value)),
        is_valid: false,
        issues: Vec::new(),
    }
}

/// 镜像 OpenAI-compatible Adapter 当前的 base URL 安全规则。
///
/// 这里负责静态、无网络校验；Adapter 构造时仍会再次检查，避免未来两个入口之一被绕过。
fn validate_endpoint(endpoint: &str) -> bool {
    let Ok(parsed) = Url::parse(endpoint) else {
        return false;
    };
    !parsed.cannot_be_a_base()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

/// 只校验本地存储形式，不联网判断 credential 是否真实有效。
///
/// 原值不 trim：首尾空白通常意味着复制错误，静默修改可能把错误凭证伪装成已修复配置。
fn valid_api_key(api_key: &str) -> bool {
    !api_key.is_empty() && api_key.trim() == api_key
}

/// Provider 是供应商身份，不是协议或 Adapter 枚举；这里仅约束为可稳定比较和展示的 key。
fn validate_provider_id(provider: &str) -> bool {
    let bytes = provider.as_bytes();
    let Some(first) = bytes.first() else {
        return false;
    };
    bytes.len() <= 64
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

/// 构造归属于合法模型 key 的固定安全诊断。
fn model_issue(
    key: &Option<ModelKey>,
    code: ConfigIssueCode,
    message: &'static str,
) -> ConfigIssue {
    ConfigIssue {
        code,
        model_key: key.clone(),
        message,
    }
}
