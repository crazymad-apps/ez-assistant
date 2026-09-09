//! 显式人工联调入口；默认忽略，凭据只读且不创建正式 Runtime／Store。
use super::*;
use crate::resources::model::HostModelServiceFactory;
use assistant_runtime::{ModelServiceFactory, ModelTokenLimit as Limit};
use std::time::Duration;

/// 人工 M0 门禁：只读显式提供的配置，以真实工厂检验必要参数；不启动 Host 或打开数据库。
/// 测试不会使用旧 TOML 中手填的限制补齐在线参数；任一连接未满足条件就整体失败。
#[tokio::test]
#[ignore = "requires an explicitly authorized EZ_MODEL_DISCOVERY_CONFIG and real Provider access"]
async fn explicitly_configured_live_connections_supply_required_model_limits() {
    use std::collections::BTreeMap;

    use sha2::{Digest, Sha256};

    let path = std::env::var_os("EZ_MODEL_DISCOVERY_CONFIG")
        .expect("set an explicitly authorized config path; no default user-home lookup");
    let source = std::fs::read(&path).expect("read authorized config");
    let configuration: toml::Value = std::str::from_utf8(&source)
        .ok()
        .and_then(|text| toml::from_str(text).ok())
        .expect("authorized config must be valid TOML; source text is never printed");
    let models = configuration["models"].as_table().expect("models table");
    // 同一服务商／地址／凭据只查询一次；key 仅作为本次内存去重依据，不进入输出。
    let mut connections = BTreeMap::<(String, String, String), Vec<String>>::new();
    for model in models.values() {
        let field = |name| {
            model
                .get(name)
                .and_then(toml::Value::as_str)
                .expect("required connection field")
                .to_owned()
        };
        connections
            .entry((field("provider"), field("endpoint"), field("api_key")))
            .or_default()
            .push(field("model"));
    }
    assert!(!connections.is_empty(), "no configured model connections");
    let home = tempfile::tempdir().expect("isolated empty Runtime Home");
    let factory = HostModelServiceFactory::new(home.path());
    let mut unmet = 0;
    for (index, ((provider, endpoint, key), selected)) in connections.into_iter().enumerate() {
        let provider = agent_types::ProviderId::new(&provider).expect("provider identifier");
        let path = match provider.as_str() {
            "dashscope" => "/compatible-mode/v1/models".to_owned(),
            "openai" => "/v1/models".to_owned(),
            _ => format!(
                "{}/models",
                reqwest::Url::parse(&endpoint)
                    .expect("config URL")
                    .path()
                    .trim_end_matches('/')
            ),
        };
        let result = factory
            .discover_models(ModelDiscoveryRequest {
                format: match provider.as_str() {
                    "moonshot" => DiscoveryFormat::Moonshot,
                    "vllm" => DiscoveryFormat::Vllm,
                    _ => DiscoveryFormat::OpenAi,
                },
                models_path: &path,
                endpoint: &endpoint,
                api_key: &key,
                connect_timeout: Duration::from_secs(5),
                request_timeout: Duration::from_secs(20),
            })
            .await;
        match result {
            Ok(discovered) => {
                let present = selected
                    .iter()
                    .all(|id| discovered.iter().any(|model| &model.model_id == id));
                let complete = selected.iter().all(|id| {
                    discovered.iter().any(|model| {
                        &model.model_id == id
                            && matches!(model.metadata.context_window_tokens, Limit::Known(_))
                            && matches!(model.metadata.max_output_tokens, Limit::Known(_))
                    })
                });
                println!(
                    "connection={} provider={} listed={} configured_present={} required_limits_complete={}",
                    index + 1,
                    provider.as_str(),
                    discovered.len(),
                    present,
                    complete
                );
                if !present || !complete {
                    unmet += 1;
                }
            }
            Err(error) => {
                println!(
                    "connection={} provider={} discovery_error={:?}",
                    index + 1,
                    provider.as_str(),
                    error.kind()
                );
                unmet += 1;
            }
        }
    }
    let after = std::fs::read(&path).expect("recheck authorized config");
    assert_eq!(
        Sha256::digest(&source),
        Sha256::digest(&after),
        "config changed during validation"
    );
    assert!(
        std::fs::read_dir(home.path())
            .expect("isolated home")
            .next()
            .is_none()
    );
    assert_eq!(
        unmet, 0,
        "M0 required-limit gate failed; list availability alone does not satisfy the design"
    );
}

/// 使用文档证实的测试专用参数验证真实单次文本调用；不将测试参数保存为用户固定配置。
#[tokio::test]
#[ignore = "requires explicitly authorized config and makes one small real model call"]
async fn live_fixed_parameters_complete_one_text_turn() {
    use agent_model::{
        GenerationConfig, ModelCallContext, ModelEvent, ModelRequest, SystemPromptSnapshot,
        ToolChoiceCapabilities, ToolImageProjection,
    };
    use agent_types::{
        ConversationMessage, ConversationSnapshot, MessageId, PartId, TextPart, ToolChoice,
        UserMessage, UserPart,
    };
    use assistant_runtime::{
        ModelParameterSource, ModelParameters, ModelProtocol, ModelServiceFactoryRequest,
        ResolvedModelCapabilities, resolve_model_parameters,
    };
    use futures_util::StreamExt;
    use sha2::{Digest, Sha256};

    let path =
        std::env::var_os("EZ_MODEL_DISCOVERY_CONFIG").expect("explicit config path required");
    let source = std::fs::read(&path).expect("read authorized config");
    let config: toml::Value = std::str::from_utf8(&source)
        .ok()
        .and_then(|value| toml::from_str(value).ok())
        .expect("valid TOML, contents never printed");
    let model = config["models"]
        .as_table()
        .expect("model table")
        .values()
        .find(|model| {
            model.get("provider").and_then(toml::Value::as_str) == Some("dashscope")
                && model.get("model").and_then(toml::Value::as_str) == Some("qwen3.8-max")
        })
        .expect("requires explicitly configured qwen3.8-max DashScope connection");
    let field = |name| {
        model
            .get(name)
            .and_then(toml::Value::as_str)
            .expect("connection field")
    };
    let home = tempfile::tempdir().expect("isolated home");
    let factory = HostModelServiceFactory::new(home.path());
    let discovered = factory
        .discover_models(ModelDiscoveryRequest {
            format: DiscoveryFormat::OpenAi,
            models_path: "/compatible-mode/v1/models",
            endpoint: field("endpoint"),
            api_key: field("api_key"),
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
        })
        .await
        .expect("live discovery, safe error");
    assert!(
        discovered
            .iter()
            .any(|entry| entry.model_id == field("model")),
        "model must come from current online list"
    );
    let known = |value| Limit::Known(std::num::NonZeroU64::new(value).expect("positive fixture"));
    // 官方 qwen3.8-max 文档的精确整数；本轮仅固定隔离内存副本，实际生成预算另设为 32。
    let fixed = ModelParameters {
        context_window_tokens: known(1_000_000),
        max_output_tokens: known(131_072),
        ..ModelParameters::default()
    };
    let frozen = resolve_model_parameters(Some(&fixed), field("model"), || async {
        panic!("fixed source must not fetch another list")
    })
    .await
    .expect("valid fixed limits");
    assert_eq!(frozen.source(), ModelParameterSource::Fixed);
    // 只验证文本；这些是本测试明确选用的能力，不从模型名称或旧配置继承能力。
    let capabilities = ResolvedModelCapabilities {
        image_input: false,
        reasoning: None,
        tool_calls: false,
        tool_image_projection: ToolImageProjection::Unsupported,
        tool_choice: ToolChoiceCapabilities {
            none: true,
            ..ToolChoiceCapabilities::default()
        },
        streaming: true,
    };
    let provider = agent_types::ProviderId::new("dashscope").expect("provider");
    let bundle = factory
        .create_model(ModelServiceFactoryRequest {
            provider: &provider,
            protocol: ModelProtocol::OpenAiChatCompletions,
            capabilities: &capabilities,
            endpoint: field("endpoint"),
            model: field("model"),
            api_key: field("api_key"),
            context_window_tokens: frozen.context_window_tokens(),
            max_input_tokens: None,
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(20),
        })
        .unwrap_or_else(|_| panic!("model construction failed; underlying errors are not printed"));
    let input = ModelRequest {
        system: SystemPromptSnapshot::default(),
        conversation: ConversationSnapshot::new(vec![ConversationMessage::User(UserMessage {
            origin: Default::default(),
            transcript_visibility: Default::default(),
            id: MessageId::new("m0-check").expect("id"),
            parts: vec![UserPart::Text(TextPart {
                id: PartId::new("m0-text").expect("id"),
                text: "Reply with only OK.".into(),
            })],
        })]),
        tools: vec![],
        tool_choice: ToolChoice::None,
        generation: GenerationConfig {
            max_output_tokens: Some(frozen.max_output_tokens().min(32)),
            ..GenerationConfig::default()
        },
        reasoning: None,
        provider_options: Default::default(),
    };
    let work = async {
        let mut stream = bundle
            .model
            .stream(input, ModelCallContext::default())
            .await
            .unwrap_or_else(|_| panic!("live stream establishment failed; details redacted"));
        let mut finished = false;
        while let Some(event) = stream.next().await {
            match event {
                ModelEvent::TurnFinished { message } => {
                    assert!(message.parts.iter().any(|part|matches!(part,agent_types::AssistantPart::Text(text) if !text.text.trim().is_empty())),"expected a nonempty text result");
                    finished = true;
                    println!(
                        "live fixed text turn finished; output_tokens={:?}",
                        message.usage.as_ref().map(|usage| usage.output_tokens)
                    );
                }
                ModelEvent::TurnFailed { .. } => panic!("live turn failed; details redacted"),
                _ => {}
            }
        }
        assert!(finished, "missing terminal result");
    };
    tokio::time::timeout(Duration::from_secs(45), work)
        .await
        .expect("bounded live call");
    assert_eq!(
        Sha256::digest(&source),
        Sha256::digest(std::fs::read(&path).expect("recheck config")),
        "config changed"
    );
    assert!(
        std::fs::read_dir(home.path())
            .expect("home")
            .next()
            .is_none()
    );
}
