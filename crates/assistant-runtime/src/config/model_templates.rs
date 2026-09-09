//! 配置详情与执行准备共用的厂商规格补齐；只填在线未知字段，不扩充目录或覆盖固定记录。
use assistant_protocol::{
    ModelConfigurationDetail, ModelConfigurationSource, ModelFeatureSupport, ModelParameters,
    ModelReasoningMode, ModelSelection, ModelTokenLimit, ModelToolImageProjection,
    ProviderConnection, ProviderType, ReasoningEffortKey,
};
use std::{collections::BTreeMap, num::NonZeroU64};

pub(crate) fn configuration_detail(
    connection: &ProviderConnection,
    selection: ModelSelection,
    mut parameters: ModelParameters,
) -> ModelConfigurationDetail {
    let (mut template, document, checked_on) =
        specification(connection.provider_type, &selection.model_id);
    // 原生工具图片是 Responses 接口能力；显式 Chat 接入仍使用后续用户图片投影。
    if connection.provider_type == ProviderType::Openai
        && template.image_input == ModelFeatureSupport::Supported
        && matches!(
            super::managed_model::resolve_provider_protocol(connection),
            Ok(super::ModelProtocol::OpenAiResponses)
        )
    {
        template.tool_image_projection = ModelToolImageProjection::NativeToolResult;
    }
    let mut sources = BTreeMap::new();
    // 只补 Unknown，Invalid 和明确不支持均是在线事实，不能被模板掩盖。
    macro_rules! fill {
        ($field:ident, $unknown:pat) => {{
            let source = if matches!(parameters.$field, $unknown) {
                if matches!(template.$field, $unknown) {
                    ModelConfigurationSource::Unconfigured
                } else {
                    parameters.$field = std::mem::take(&mut template.$field);
                    ModelConfigurationSource::Template
                }
            } else {
                ModelConfigurationSource::Online
            };
            sources.insert(stringify!($field).to_owned(), source);
        }};
    }
    fill!(context_window_tokens, ModelTokenLimit::Unknown);
    fill!(max_input_tokens, ModelTokenLimit::Unknown);
    fill!(max_output_tokens, ModelTokenLimit::Unknown);
    fill!(reasoning_max_input_tokens, ModelTokenLimit::Unknown);
    fill!(reasoning_max_output_tokens, ModelTokenLimit::Unknown);
    fill!(streaming, ModelFeatureSupport::Unknown);
    fill!(image_input, ModelFeatureSupport::Unknown);
    fill!(tool_calls, ModelFeatureSupport::Unknown);
    fill!(tool_image_projection, ModelToolImageProjection::Unknown);
    for (name, value, fallback) in [
        (
            "tool_choice.auto",
            &mut parameters.tool_choice.auto,
            template.tool_choice.auto,
        ),
        (
            "tool_choice.none",
            &mut parameters.tool_choice.none,
            template.tool_choice.none,
        ),
    ] {
        let source = if *value != ModelFeatureSupport::Unknown {
            ModelConfigurationSource::Online
        } else if fallback != ModelFeatureSupport::Unknown {
            *value = fallback;
            ModelConfigurationSource::Template
        } else {
            ModelConfigurationSource::Unconfigured
        };
        sources.insert(name.into(), source);
    }
    // 在线已否定思考时，不能用模板强度制造矛盾配置。
    if parameters.reasoning != ModelFeatureSupport::Unsupported
        && parameters.reasoning_mode != ModelReasoningMode::Unsupported
    {
        fill!(reasoning, ModelFeatureSupport::Unknown);
        fill!(reasoning_mode, ModelReasoningMode::Unknown);
        let effort_source = if parameters.reasoning_efforts.is_some() {
            ModelConfigurationSource::Online
        } else if template.reasoning_efforts.is_some() {
            parameters.reasoning_efforts = template.reasoning_efforts.take();
            ModelConfigurationSource::Template
        } else {
            ModelConfigurationSource::Unconfigured
        };
        sources.insert("reasoning_efforts".into(), effort_source);
        if parameters.default_reasoning_effort.is_none()
            && template.default_reasoning_effort.is_some_and(|key| {
                parameters
                    .reasoning_efforts
                    .as_ref()
                    .is_some_and(|values| values.contains_key(&key))
            })
        {
            parameters.default_reasoning_effort = template.default_reasoning_effort;
            sources.insert(
                "default_reasoning_effort".into(),
                ModelConfigurationSource::Template,
            );
        }
    }
    ModelConfigurationDetail {
        origin: assistant_protocol::ModelConfigOrigin::Online,
        selection,
        parameters,
        source: ModelConfigurationSource::Online,
        updated_at_ms: None,
        field_sources: sources,
        template_document: document.map(str::to_owned),
        template_checked_on: checked_on.map(str::to_owned),
    }
}

fn known(value: u64) -> ModelTokenLimit {
    ModelTokenLimit::Known(NonZeroU64::new(value).expect("documented positive token count"))
}

/// 精确匹配已核查型号，部署／中转不按名称推断限额；没有整数依据的 K/M 字段保持未知。
fn specification(
    provider: ProviderType,
    model: &str,
) -> (ModelParameters, Option<&'static str>, Option<&'static str>) {
    use ModelFeatureSupport::{Supported, Unsupported};
    use ReasoningEffortKey::{High, Low, Max, Medium, XHigh};
    let mut spec = ModelParameters::default();
    let (document, checked_on) = match (provider, model) {
        (
            ProviderType::Openai,
            "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-5.5"
            | "gpt-5.5-2026-04-23" | "gpt-6-astra",
        ) => {
            // 官方 API 整数规格；Codex 客户端压缩阈值不是模型窗口，反代限额可由固定配置覆盖。
            spec.context_window_tokens = known(1_050_000);
            spec.max_output_tokens = known(128_000);
            spec.image_input = Supported;
            spec.tool_calls = Supported;
            spec.reasoning = Supported;
            spec.reasoning_mode = if model == "gpt-6-astra" {
                ModelReasoningMode::Always
            } else {
                ModelReasoningMode::Optional
            };
            let mut efforts = BTreeMap::from([
                (Low, "low".into()),
                (Medium, "medium".into()),
                (High, "high".into()),
                (XHigh, "xhigh".into()),
            ]);
            if matches!(
                model,
                "gpt-5.6" | "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" | "gpt-6-astra"
            ) {
                efforts.insert(Max, "max".into());
            }
            spec.reasoning_efforts = Some(efforts);
            spec.default_reasoning_effort = Some(Medium);
            (
                match model {
                    "gpt-6-astra" => "https://developers.openai.com/api/docs/models/gpt-6-astra",
                    "gpt-5.6-terra" => {
                        "https://developers.openai.com/api/docs/models/gpt-5.6-terra"
                    }
                    "gpt-5.6-luna" => "https://developers.openai.com/api/docs/models/gpt-5.6-luna",
                    "gpt-5.5" | "gpt-5.5-2026-04-23" => {
                        "https://developers.openai.com/api/docs/models/gpt-5.5"
                    }
                    _ => "https://developers.openai.com/api/docs/models/gpt-5.6-sol",
                },
                "2026-09-09",
            )
        }
        (ProviderType::Openai, "gpt-5.3-codex-spark") => {
            // Codex 官方客户端模型元数据确认 128000 窗口和以下档位；未公开的输出上限保持未知。
            // Spark 是纯文本型号，不能套用 GPT-5.3-Codex 的图片能力或 400000 窗口。
            spec.context_window_tokens = known(128_000);
            spec.image_input = Unsupported;
            spec.tool_image_projection = ModelToolImageProjection::Unsupported;
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Always;
            spec.reasoning_efforts = Some(
                [
                    (Low, "low".into()),
                    (Medium, "medium".into()),
                    (High, "high".into()),
                    (XHigh, "xhigh".into()),
                ]
                .into(),
            );
            spec.default_reasoning_effort = Some(High);
            ("https://learn.chatgpt.com/docs/models", "2026-09-09")
        }
        (ProviderType::Openai, "gpt-4.1" | "gpt-4.1-2025-04-14") => {
            spec.context_window_tokens = known(1_047_576);
            spec.max_output_tokens = known(32_768);
            spec.streaming = Supported;
            spec.image_input = Supported;
            spec.tool_calls = Supported;
            spec.reasoning = Unsupported;
            spec.reasoning_mode = ModelReasoningMode::Unsupported;
            (
                "https://developers.openai.com/api/docs/models/gpt-4.1",
                "2026-09-09",
            )
        }
        (
            ProviderType::DashscopeApi | ProviderType::DashscopePlan,
            "qwen3.8-max"
            | "qwen3.8-max-0902"
            | "qwen3.8-max-2026-09-02"
            | "qwen3.8-flash"
            | "qwen3.7-max"
            | "qwen3.7-plus"
            | "qwen3.6-flash",
        ) => {
            spec.context_window_tokens = known(1_000_000);
            spec.max_input_tokens = known(991_808);
            let output = if model == "qwen3.6-flash" {
                65_536
            } else {
                131_072
            };
            spec.max_output_tokens = known(output);
            spec.reasoning_max_input_tokens = known(983_616);
            spec.reasoning_max_output_tokens = known(output);
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Optional;
            // Qwen 3.6/3.7 支持 thinking_budget，不能据此虚构字符串强度档位。
            if model.starts_with("qwen3.8-") {
                spec.reasoning_efforts = Some(
                    [
                        (Low, "low".into()),
                        (Medium, "medium".into()),
                        (XHigh, "xhigh".into()),
                    ]
                    .into(),
                );
                spec.default_reasoning_effort = Some(XHigh);
            }
            spec.image_input = if model == "qwen3.7-max" {
                Unsupported
            } else {
                Supported
            };
            spec.tool_calls = Supported;
            (
                match model {
                    "qwen3.8-flash" => "https://help.aliyun.com/zh/model-studio/qwen3-8-flash",
                    "qwen3.7-max" => "https://help.aliyun.com/zh/model-studio/qwen3-7-max",
                    "qwen3.7-plus" => "https://help.aliyun.com/zh/model-studio/qwen3-7-plus",
                    "qwen3.6-flash" => "https://help.aliyun.com/zh/model-studio/qwen3-6-flash",
                    _ => "https://help.aliyun.com/zh/model-studio/qwen3-8-max",
                },
                "2026-09-09",
            )
        }
        (ProviderType::DashscopeApi | ProviderType::DashscopePlan, "glm-5.2") => {
            // 百炼托管规格，不套用智谱原厂同名型号的参数。
            spec.context_window_tokens = known(1_048_576);
            spec.max_input_tokens = known(1_048_576);
            spec.max_output_tokens = known(131_072);
            spec.reasoning_max_input_tokens = known(1_048_576);
            spec.reasoning_max_output_tokens = known(131_072);
            spec.image_input = Unsupported;
            spec.tool_calls = Supported;
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Optional;
            spec.reasoning_efforts = Some(
                [
                    (Low, "low".into()),
                    (Medium, "medium".into()),
                    (High, "high".into()),
                    (XHigh, "xhigh".into()),
                    (Max, "max".into()),
                ]
                .into(),
            );
            spec.default_reasoning_effort = Some(High);
            (
                "https://help.aliyun.com/zh/model-studio/glm-5-2",
                "2026-09-09",
            )
        }
        (
            ProviderType::DashscopeApi | ProviderType::DashscopePlan,
            "deepseek-v4-pro" | "deepseek-v4-flash-0731",
        ) => {
            spec.context_window_tokens = known(1_000_000);
            spec.max_input_tokens = known(1_000_000);
            // 百炼明确给出整数 393216，包含思考输出；不沿用原厂的运行预算 384000。
            spec.max_output_tokens = known(393_216);
            spec.reasoning_max_input_tokens = known(1_000_000);
            spec.reasoning_max_output_tokens = known(393_216);
            spec.image_input = Unsupported;
            spec.tool_calls = Supported;
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Optional;
            let mut efforts = BTreeMap::from([(High, "high".into()), (Max, "max".into())]);
            if model == "deepseek-v4-flash-0731" {
                efforts.insert(Low, "low".into());
            }
            spec.reasoning_efforts = Some(efforts);
            spec.default_reasoning_effort = Some(High);
            (
                if model == "deepseek-v4-pro" {
                    "https://help.aliyun.com/zh/model-studio/deepseek-v4-pro"
                } else {
                    "https://help.aliyun.com/zh/model-studio/deepseek-v4-flash"
                },
                "2026-09-09",
            )
        }
        (
            ProviderType::Moonshot,
            "k3" | "k3-256k" | "kimi-for-coding" | "kimi-for-coding-highspeed",
        ) => {
            spec.context_window_tokens = known(if model == "k3" { 1_048_576 } else { 262_144 });
            // Coding 官方客户端将请求输出预算封顶 128k；这是可编辑预算，不宣称独立硬上限。
            spec.max_output_tokens = known(131_072);
            spec.image_input = Supported;
            spec.tool_calls = Supported;
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Always;
            spec.reasoning_efforts = Some(
                [
                    (Low, "low".into()),
                    (High, "high".into()),
                    (Max, "max".into()),
                ]
                .into(),
            );
            spec.default_reasoning_effort = Some(High);
            if !model.starts_with("k3") {
                spec.reasoning_efforts = None;
                spec.default_reasoning_effort = None;
            }
            (
                "https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/config-files",
                "2026-09-09",
            )
        }
        (
            ProviderType::Deepseek,
            "deepseek-v4-flash" | "deepseek-v4-pro" | "deepseek-v4-flash-vision-exp",
        ) => {
            // 官方集成示例提供明确整数；作为可编辑运行预填，不冒充实测硬上限。
            let document = if model == "deepseek-v4-flash-vision-exp" {
                spec.context_window_tokens = known(1_048_576);
                // 中文规格表明确三型号共用 384K；沿用官方 Pi 示例的运行预算整数。
                spec.max_output_tokens = known(384_000);
                spec.image_input = Supported;
                "https://api-docs.deepseek.com/zh-cn/quick_start/pricing/"
            } else {
                spec.context_window_tokens = known(1_000_000);
                spec.max_output_tokens = known(384_000);
                spec.image_input = Unsupported;
                "https://api-docs.deepseek.com/quick_start/agent_integrations/pi_mono/"
            };
            spec.tool_calls = Supported;
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Optional;
            spec.reasoning_efforts = Some(
                [
                    (Low, "low".into()),
                    (High, "high".into()),
                    (Max, "max".into()),
                ]
                .into(),
            );
            spec.default_reasoning_effort = Some(High);
            (document, "2026-09-09")
        }
        (ProviderType::Zhipu, "glm-5.3" | "glm-5.3-flash") => {
            spec.context_window_tokens = known(1_000_000);
            // 官方说明 128K，示例以 65536 作为运行预算；模板采用该可直接使用的预算。
            spec.max_output_tokens = known(65_536);
            spec.tool_calls = Supported;
            spec.image_input = if model == "glm-5.3-flash" {
                Supported
            } else {
                Unsupported
            };
            spec.reasoning = Supported;
            spec.reasoning_mode = ModelReasoningMode::Always;
            spec.reasoning_efforts = Some(
                [
                    (Low, "low".into()),
                    (High, "high".into()),
                    (Max, "max".into()),
                ]
                .into(),
            );
            spec.default_reasoning_effort = Some(Max);
            (
                if model == "glm-5.3-flash" {
                    "https://docs.bigmodel.cn/cn/guide/models/vlm/glm-5.3-flash"
                } else {
                    "https://docs.bigmodel.cn/cn/guide/models/text/glm-5.3"
                },
                "2026-09-09",
            )
        }
        _ => return (spec, None, None),
    };
    // 本表仅收录已核查的流式对话型号；未命中分支已返回，不向未知模型推断能力。
    spec.streaming = Supported;
    if spec.tool_calls == Supported {
        spec.tool_choice.auto = Supported;
        spec.tool_choice.none = Supported;
    }
    if spec.image_input == Supported {
        spec.tool_image_projection = ModelToolImageProjection::FollowUpUserMessage;
    }
    (spec, Some(document), Some(checked_on))
}
