//! 执行前的参数来源解析。仅消费调用方已按服务商／模型定位的固定快照，不访问配置文件或数据库。

use std::future::Future;

use thiserror::Error;

use super::{
    DiscoveredModel, ModelDiscoveryError, ModelFeatureSupport, ModelParameters, ModelReasoningMode,
    ModelTokenLimit,
};

/// 当前执行规格的来源；固定配置与在线列表的生命周期不同。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelParameterSource {
    Fixed,
    Online,
}

/// 已通过数值／组合校验的不可变快照；能力未知仍保留，后续装配必须按用途继续校验。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedModelParameters {
    source: ModelParameterSource,
    parameters: ModelParameters,
    context_window_tokens: u64,
    max_output_tokens: u32,
}

impl ResolvedModelParameters {
    pub fn source(&self) -> ModelParameterSource {
        self.source
    }
    pub fn parameters(&self) -> &ModelParameters {
        &self.parameters
    }
    pub fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }
    pub fn max_output_tokens(&self) -> u32 {
        self.max_output_tokens
    }
}

/// 只包含字段名／固定消息，不包含模型响应或凭据；可由 UI 定位待编辑字段。
#[derive(Debug, Error)]
pub enum ModelParameterError {
    #[error("required model parameter is missing: {0}")]
    MissingRequired(&'static str),
    #[error("invalid model parameter: {0}")]
    Invalid(&'static str),
    #[error("model reasoning settings are inconsistent")]
    InconsistentReasoning,
    #[error("model tool and image settings are inconsistent")]
    InconsistentTools,
    #[error("selected model is absent from the current online result")]
    ModelNotFound,
    #[error("model discovery failed")]
    Discovery(#[source] ModelDiscoveryError),
}

/// 解析调用方冻结的来源：有固定记录时不创建在线 future，不混入在线字段；否则仅使用本次结果。
///
/// 调用方必须先按服务商实例＋模型 ID 查找 fixed，并在结果接纳前校验服务商和固定记录仍有效。
/// 本函数不提交选择、不建立缓存；丢弃 future 会丢弃未完成的在线请求。重置后的调用传入 None。
///
/// # Errors
/// 返回字段缺失／非法、思考设置冲突、目录缺模型或在线发现错误；非法固定值绝不回退在线。
pub async fn resolve_model_parameters<F, Fut>(
    fixed: Option<&ModelParameters>,
    model_id: &str,
    fetch_online: F,
) -> Result<ResolvedModelParameters, ModelParameterError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<DiscoveredModel>, ModelDiscoveryError>>,
{
    if let Some(parameters) = fixed {
        return validate(parameters.clone(), ModelParameterSource::Fixed);
    }
    let models = fetch_online()
        .await
        .map_err(ModelParameterError::Discovery)?;
    let model = models
        .into_iter()
        .find(|model| model.model_id == model_id)
        .ok_or(ModelParameterError::ModelNotFound)?;
    validate(model.metadata, ModelParameterSource::Online)
}

/// 用户固定记录保存和执行共用的完整参数校验；无网络、无默认值补齐。
pub fn validate_fixed_model_parameters(
    parameters: &ModelParameters,
) -> Result<ResolvedModelParameters, ModelParameterError> {
    validate(parameters.clone(), ModelParameterSource::Fixed)
}

fn validate(
    parameters: ModelParameters,
    source: ModelParameterSource,
) -> Result<ResolvedModelParameters, ModelParameterError> {
    let context = required(parameters.context_window_tokens, "context_window_tokens")?;
    let output = required(parameters.max_output_tokens, "max_output_tokens")?;
    // GenerationConfig 使用 u32；不能等构造请求时才截断已保存的输出限制。
    let output =
        u32::try_from(output).map_err(|_| ModelParameterError::Invalid("max_output_tokens"))?;
    if context > 9_007_199_254_740_991 || u64::from(output) > context {
        return Err(ModelParameterError::Invalid(
            "context_window_tokens/max_output_tokens",
        ));
    }
    for (name, limit) in [
        ("max_input_tokens", parameters.max_input_tokens),
        (
            "reasoning_max_input_tokens",
            parameters.reasoning_max_input_tokens,
        ),
        (
            "reasoning_max_output_tokens",
            parameters.reasoning_max_output_tokens,
        ),
    ] {
        match limit {
            ModelTokenLimit::Invalid => return Err(ModelParameterError::Invalid(name)),
            ModelTokenLimit::Known(value)
                if value.get() > context
                    || (name == "reasoning_max_output_tokens"
                        && value.get() > u64::from(u32::MAX)) =>
            {
                return Err(ModelParameterError::Invalid(name));
            }
            _ => {}
        }
    }
    let has_reasoning_settings = matches!(
        parameters.reasoning_mode,
        ModelReasoningMode::Optional | ModelReasoningMode::Always
    ) || parameters
        .reasoning_efforts
        .as_ref()
        .is_some_and(|efforts| !efforts.is_empty());
    if (parameters.reasoning == ModelFeatureSupport::Unsupported && has_reasoning_settings)
        || (parameters.reasoning_mode == ModelReasoningMode::Unsupported
            && (parameters.reasoning == ModelFeatureSupport::Supported || has_reasoning_settings))
    {
        return Err(ModelParameterError::InconsistentReasoning);
    }
    if (parameters.reasoning == ModelFeatureSupport::Unsupported
        || parameters.reasoning_mode == ModelReasoningMode::Unsupported)
        && (matches!(
            parameters.reasoning_max_input_tokens,
            ModelTokenLimit::Known(_)
        ) || matches!(
            parameters.reasoning_max_output_tokens,
            ModelTokenLimit::Known(_)
        ))
    {
        return Err(ModelParameterError::InconsistentReasoning);
    }
    use assistant_protocol::ModelToolImageProjection;
    let has_tool_image = matches!(
        parameters.tool_image_projection,
        ModelToolImageProjection::NativeToolResult | ModelToolImageProjection::FollowUpUserMessage
    );
    let has_tool_choice = [
        parameters.tool_choice.auto,
        parameters.tool_choice.required,
        parameters.tool_choice.named,
    ]
    .contains(&ModelFeatureSupport::Supported);
    if (parameters.tool_calls == ModelFeatureSupport::Unsupported
        && (has_tool_choice || has_tool_image))
        || (parameters.image_input == ModelFeatureSupport::Unsupported && has_tool_image)
    {
        return Err(ModelParameterError::InconsistentTools);
    }
    if let Some(efforts) = &parameters.reasoning_efforts {
        if efforts.len() > 5
            || efforts.values().any(|value| {
                value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control)
            })
            || parameters
                .default_reasoning_effort
                .is_some_and(|default| !efforts.contains_key(&default))
            || (!efforts.is_empty() && parameters.default_reasoning_effort.is_none())
        {
            return Err(ModelParameterError::InconsistentReasoning);
        }
    } else if parameters.default_reasoning_effort.is_some() {
        return Err(ModelParameterError::InconsistentReasoning);
    }
    Ok(ResolvedModelParameters {
        source,
        parameters,
        context_window_tokens: context,
        max_output_tokens: output,
    })
}

fn required(value: ModelTokenLimit, field: &'static str) -> Result<u64, ModelParameterError> {
    match value {
        ModelTokenLimit::Known(value) => Ok(value.get()),
        ModelTokenLimit::Unknown => Err(ModelParameterError::MissingRequired(field)),
        ModelTokenLimit::Invalid => Err(ModelParameterError::Invalid(field)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReasoningEffortKey;
    use std::num::NonZeroU64;

    fn known(value: u64) -> ModelTokenLimit {
        ModelTokenLimit::Known(NonZeroU64::new(value).expect("positive fixture"))
    }
    fn values() -> ModelParameters {
        ModelParameters {
            context_window_tokens: known(8192),
            max_output_tokens: known(1024),
            ..ModelParameters::default()
        }
    }
    fn offline() -> std::future::Ready<Result<Vec<DiscoveredModel>, ModelDiscoveryError>> {
        panic!("fixed configuration must not invoke online discovery")
    }
    fn online(
        parameters: ModelParameters,
    ) -> std::future::Ready<Result<Vec<DiscoveredModel>, ModelDiscoveryError>> {
        std::future::ready(Ok(vec![DiscoveredModel {
            configuration: None,
            model_id: "model".into(),
            display_name: None,
            metadata: parameters,
        }]))
    }

    #[tokio::test]
    async fn fixed_snapshot_never_fetches_or_merges_unknown_capabilities() {
        let mut fixed = values();
        let frozen = resolve_model_parameters(Some(&fixed), "model", offline)
            .await
            .expect("fixed");
        fixed.max_output_tokens = known(128);
        assert_eq!(frozen.source(), ModelParameterSource::Fixed);
        assert_eq!(frozen.max_output_tokens(), 1024);
        assert_eq!(
            frozen.parameters().image_input,
            ModelFeatureSupport::Unknown
        );
        assert_eq!(
            resolve_model_parameters(Some(&fixed), "model", offline)
                .await
                .expect("next run")
                .max_output_tokens(),
            128
        );
    }

    #[tokio::test]
    async fn reset_returns_to_current_online_parameters_or_reports_missing_values() {
        let resolved = resolve_model_parameters(None, "model", || online(values()))
            .await
            .expect("online");
        assert_eq!(resolved.source(), ModelParameterSource::Online);
        assert!(matches!(
            resolve_model_parameters(None, "model", || online(ModelParameters::default())).await,
            Err(ModelParameterError::MissingRequired(
                "context_window_tokens"
            ))
        ));
        assert!(matches!(
            resolve_model_parameters(None, "absent", || online(values())).await,
            Err(ModelParameterError::ModelNotFound)
        ));
        assert!(matches!(
            resolve_model_parameters(None, "model", || std::future::ready(Err(
                ModelDiscoveryError::new(super::super::ModelDiscoveryErrorKind::Timeout)
            )))
            .await,
            Err(ModelParameterError::Discovery(_))
        ));
    }

    #[tokio::test]
    async fn invalid_fixed_limits_fail_without_network_fallback() {
        for output in [
            ModelTokenLimit::Unknown,
            ModelTokenLimit::Invalid,
            known(8193),
            known(u64::from(u32::MAX) + 1),
        ] {
            let mut fixed = values();
            fixed.max_output_tokens = output;
            assert!(
                resolve_model_parameters(Some(&fixed), "model", offline)
                    .await
                    .is_err()
            );
        }
        let mut fixed = values();
        fixed.max_input_tokens = known(8192);
        assert!(
            resolve_model_parameters(Some(&fixed), "model", offline)
                .await
                .is_ok(),
            "independent maxima are not summed"
        );
        fixed.reasoning_max_output_tokens = ModelTokenLimit::Invalid;
        assert!(
            resolve_model_parameters(Some(&fixed), "model", offline)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reasoning_defaults_and_disabled_mode_cannot_contradict_fixed_capabilities() {
        let mut fixed = values();
        fixed.reasoning = ModelFeatureSupport::Supported;
        fixed.reasoning_mode = ModelReasoningMode::Always;
        fixed.reasoning_efforts = Some(
            [
                (ReasoningEffortKey::Low, "low".into()),
                (ReasoningEffortKey::High, "high".into()),
            ]
            .into(),
        );
        fixed.default_reasoning_effort = Some(ReasoningEffortKey::High);
        assert!(
            resolve_model_parameters(Some(&fixed), "model", offline)
                .await
                .is_ok()
        );
        fixed.default_reasoning_effort = Some(ReasoningEffortKey::Max);
        assert!(matches!(
            resolve_model_parameters(Some(&fixed), "model", offline).await,
            Err(ModelParameterError::InconsistentReasoning)
        ));
        fixed.default_reasoning_effort = Some(ReasoningEffortKey::High);
        fixed.reasoning = ModelFeatureSupport::Unsupported;
        assert!(
            resolve_model_parameters(Some(&fixed), "model", offline)
                .await
                .is_err()
        );
    }

    #[test]
    fn declared_disabled_features_reject_related_enabled_settings() {
        let mut fixed = values();
        fixed.reasoning = ModelFeatureSupport::Unsupported;
        fixed.reasoning_max_output_tokens = known(512);
        assert!(matches!(
            validate_fixed_model_parameters(&fixed),
            Err(ModelParameterError::InconsistentReasoning)
        ));
        fixed = values();
        fixed.tool_calls = ModelFeatureSupport::Unsupported;
        fixed.tool_choice.required = ModelFeatureSupport::Supported;
        assert!(matches!(
            validate_fixed_model_parameters(&fixed),
            Err(ModelParameterError::InconsistentTools)
        ));
        fixed.tool_choice.required = ModelFeatureSupport::Unknown;
        // None 仍可用于文本请求，不能把它误认为启用了工具。
        fixed.tool_choice.none = ModelFeatureSupport::Supported;
        assert!(validate_fixed_model_parameters(&fixed).is_ok());
        fixed.tool_image_projection =
            assistant_protocol::ModelToolImageProjection::FollowUpUserMessage;
        assert!(matches!(
            validate_fixed_model_parameters(&fixed),
            Err(ModelParameterError::InconsistentTools)
        ));
        fixed.tool_calls = ModelFeatureSupport::Supported;
        fixed.image_input = ModelFeatureSupport::Unsupported;
        assert!(matches!(
            validate_fixed_model_parameters(&fixed),
            Err(ModelParameterError::InconsistentTools)
        ));
        fixed.image_input = ModelFeatureSupport::Supported;
        assert!(validate_fixed_model_parameters(&fixed).is_ok());
    }
}
