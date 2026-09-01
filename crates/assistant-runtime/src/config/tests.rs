use super::domain::ModelSecret;
use super::*;
use agent_core::{ActiveGuardrailMode, ExecutionBudget};
use assistant_protocol::ModelKey;
use std::time::Duration;

const SECRET: &str = "unique-test-secret-9f1ca2";

fn model_table(key: &str, provider: &str, max_output_tokens: u32) -> String {
    format!(
        r#"
[models.{key}]
protocol = "chat_completions"
provider = "{provider}"
endpoint = "https://api.example.test/v1"
model = "model-name"
api_key = "{SECRET}"
context_window_tokens = 128000
max_output_tokens = {max_output_tokens}
"#
    )
}

fn model_catalog() -> ModelCatalog {
    ModelCatalog::from_json(
        r#"{
            "schema_version": 2,
            "catalog_revision": "fixture",
            "models": [
                {
                    "provider": "deepseek",
                    "protocol": "openai_chat_completions",
                    "model_ids": ["fixture-reasoner"],
                    "capabilities": {
                        "reasoning": {
                            "enabled": true,
                            "default_effort": "max",
                            "effort_map": {
                                "high": {"label": "High", "wire_value": "high"},
                                "max": {"label": "Max", "wire_value": "max"}
                            }
                        }
                    }
                },
                {
                    "provider": "dashscope",
                    "protocol": "openai_chat_completions",
                    "model_ids": ["qwen3.8-max"],
                    "capabilities": {
                        "image_input": true,
                        "tool_image_projection": "aggregated_user_input",
                        "tool_calls": true,
                        "streaming": true,
                        "reasoning": {
                            "enabled": true,
                            "default_effort": "xhigh",
                            "effort_map": {
                                "low": {"label": "低", "wire_value": "low"},
                                "medium": {"label": "中", "wire_value": "medium"},
                                "xhigh": {"label": "超高", "wire_value": "xhigh"}
                            }
                        }
                    }
                }
            ]
        }"#,
    )
    .expect("catalog")
}

#[test]
fn host_speech_table_is_tolerated_but_not_compiled_into_runtime_config() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"fixture\"\n{}\n\
         [speech.asr]\nprovider = \"dashscope\"\nmodel = \"asr\"\ncredential = \"secret\"\ntimeout_ms = 30000\n",
        model_table("fixture", "fixture", 4096)
    );
    let compilation = compile_runtime_config(&document);
    assert!(compilation.active().is_some());
    assert!(compilation.projection().issues.is_empty());
}

#[test]
fn catalog_match_and_complete_reasoning_override_follow_precedence() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"reasoner\"\n{}",
        model_table("reasoner", "deepseek", 4096).replace("model-name", "fixture-reasoner")
    );
    let compilation = compile_runtime_config_with_catalog(&document, &model_catalog());
    let model = compilation
        .active()
        .and_then(|active| active.model(&ModelKey::new("reasoner").expect("key")))
        .expect("model");
    let reasoning = model.capabilities().reasoning.as_ref().expect("reasoning");
    assert_eq!(reasoning.default_effort, Some(ReasoningEffortKey::Max));
    assert_eq!(reasoning.efforts.len(), 2);

    let overridden = format!(
        "{document}\n[models.reasoner.capabilities]\nimage_input = true\n\
         [models.reasoner.capabilities.reasoning]\nenabled = false\n"
    );
    let compilation = compile_runtime_config_with_catalog(&overridden, &model_catalog());
    let model = compilation
        .active()
        .and_then(|active| active.model(&ModelKey::new("reasoner").expect("key")))
        .expect("overridden model");
    assert!(model.capabilities().image_input);
    assert!(!model.capabilities().reasoning_enabled());
}

#[test]
fn invalid_override_effort_fails_only_target_model() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"reasoner\"\n{}\n\
         [models.reasoner.capabilities.reasoning]\n\
         enabled = true\n\
         default_effort = \"max\"\n\
         [models.reasoner.capabilities.reasoning.effort_map.high]\n\
         label = \"High\"\n\
         wire_value = \"high\"\n",
        model_table("reasoner", "deepseek", 4096).replace("model-name", "fixture-reasoner")
    );
    let compilation = compile_runtime_config_with_catalog(&document, &model_catalog());
    assert_eq!(compilation.state(), ConfigState::Degraded);
    assert!(compilation.active().expect("active").models().is_empty());
    assert!(compilation.projection().issues.iter().any(|issue| {
        issue.code() == ConfigIssueCode::InvalidModel
            && issue
                .model_key()
                .is_some_and(|key| key.as_str() == "reasoner")
    }));
}

#[test]
fn complete_configuration_compiles_to_existing_contracts() {
    let document = format!(
        r#"
schema_version = 1
default_model = "deepseek-chat"

[runtime.model_transport]
connect_timeout_ms = 5000
request_timeout_ms = 120000

[runtime.model_retry]
retry_on = ["connection", "timeout", "rate_limited", "unavailable"]
delays_ms = [100, 200]
max_retry_after_ms = 1000

[agent.defaults.generation]
max_output_tokens = 4096
stop = ["END"]

[agent.defaults.execution_limits]
max_steps = 40
max_tool_calls = 100
{}
"#,
        model_table("deepseek-chat", "deepseek", 8192)
    );

    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Ready);
    let active = compilation.active().expect("active configuration");
    assert_eq!(active.transport().connect_timeout(), Duration::from_secs(5));
    assert_eq!(
        active.transport().request_timeout(),
        Duration::from_secs(120)
    );
    assert_eq!(active.budget().max_steps, Some(40));
    assert_eq!(active.budget().max_tool_calls, Some(100));
    assert_eq!(active.retry_policy().expect("retry policy").delays.len(), 2);
    let model = active
        .model(&ModelKey::new("deepseek-chat").expect("model key"))
        .expect("compiled model");
    assert_eq!(model.provider().as_str(), "deepseek");
    assert_eq!(model.protocol(), ModelProtocol::OpenAiChatCompletions);
    assert_eq!(model.context_window_tokens(), 128_000);
    assert_eq!(model.max_output_tokens(), 8_192);
    assert_eq!(model.generation().max_output_tokens, Some(4_096));
    assert_eq!(model.generation().stop, ["END"]);
    assert_eq!(model.api_key(), SECRET);
}

#[test]
fn omitted_optional_tables_keep_defaults_and_no_hidden_limits() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"standard\"\n{}",
        model_table("standard", "302-ai", 4096)
    );

    let compilation = compile_runtime_config(&document);
    let active = compilation.active().expect("active configuration");
    assert_eq!(compilation.state(), ConfigState::Ready);
    assert_eq!(
        active.transport().connect_timeout(),
        Duration::from_secs(10)
    );
    assert_eq!(
        active.transport().request_timeout(),
        Duration::from_secs(300)
    );
    assert!(active.retry_policy().is_none());
    assert_eq!(active.budget(), &ExecutionBudget::default());
    assert_eq!(
        active
            .guardrails()
            .repeated_invocation
            .expect("default repeated guardrail")
            .threshold
            .get(),
        4
    );
    assert_eq!(
        active
            .guardrails()
            .consecutive_failures
            .expect("default failure guardrail")
            .threshold
            .get(),
        5
    );
    let model = active.models().values().next().expect("compiled model");
    assert_eq!(model.provider().as_str(), "302-ai");
    assert_eq!(model.protocol(), ModelProtocol::OpenAiChatCompletions);
    assert_eq!(model.generation().max_output_tokens, Some(4_096));

    let delegation = active.delegation();
    assert_eq!(delegation.max_tasks_per_run().get(), 8);
    assert_eq!(delegation.max_concurrent_tasks().get(), 4);
    assert_eq!(delegation.task_timeout(), Duration::from_secs(900));
    assert_eq!(delegation.max_steps().get(), 40);
    assert_eq!(delegation.max_tool_calls().get(), 100);
    assert_eq!(delegation.max_output_tokens().get(), 16_384);
}

#[test]
fn responses_protocol_requires_its_unique_config_value_and_uses_conservative_capabilities() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"responses\"\n{}",
        model_table("responses", "fixture", 4096).replace(
            "protocol = \"chat_completions\"",
            "protocol = \"openai_responses\""
        )
    );
    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Ready);
    let model = compilation
        .active()
        .and_then(|active| active.model(&ModelKey::new("responses").expect("key")))
        .expect("Responses model");
    assert_eq!(model.protocol(), ModelProtocol::OpenAiResponses);
    assert!(!model.capabilities().image_input);
    assert!(model.capabilities().tool_calls);
    assert_eq!(
        model.capabilities().tool_choice,
        agent_model::ToolChoiceCapabilities::auto_only()
    );

    let alias = document.replace("openai_responses", "responses");
    let rejected = compile_runtime_config(&alias);
    assert_eq!(rejected.state(), ConfigState::Degraded);
    assert!(
        rejected
            .projection()
            .issues
            .iter()
            .any(|issue| { issue.code() == ConfigIssueCode::UnsupportedProtocol })
    );
}

#[test]
fn delegation_limits_compile_and_reject_invalid_combinations() {
    let document = format!(
        r#"
schema_version = 1
default_model = "standard"

[agent.defaults.delegation]
max_tasks_per_run = 6
max_concurrent_tasks = 3
task_timeout_ms = 120000
max_steps = 20
max_tool_calls = 30
max_output_tokens = 2048
{}
"#,
        model_table("standard", "acme", 4096)
    );
    let compilation = compile_runtime_config(&document);
    let delegation = compilation
        .active()
        .expect("valid delegation configuration")
        .delegation();
    assert_eq!(delegation.max_tasks_per_run().get(), 6);
    assert_eq!(delegation.max_concurrent_tasks().get(), 3);
    assert_eq!(delegation.task_timeout(), Duration::from_secs(120));
    assert_eq!(delegation.max_steps().get(), 20);
    assert_eq!(delegation.max_tool_calls().get(), 30);
    assert_eq!(delegation.max_output_tokens().get(), 2_048);
    assert_eq!(compilation.projection().delegation, Some(delegation));

    for invalid_fields in [
        "max_tasks_per_run = 0\nmax_concurrent_tasks = 1",
        "max_tasks_per_run = 2\nmax_concurrent_tasks = 3",
        "max_tasks_per_run = 2\nmax_concurrent_tasks = 1\ntask_timeout_ms = 0",
    ] {
        let invalid = compile_runtime_config(&format!(
            "schema_version = 1\ndefault_model = \"standard\"\n[agent.defaults.delegation]\n{invalid_fields}\n{}",
            model_table("standard", "acme", 4096)
        ));
        assert_eq!(invalid.state(), ConfigState::Invalid);
        assert!(
            invalid
                .issues()
                .iter()
                .any(|issue| issue.code() == ConfigIssueCode::InvalidLimit)
        );
    }

    let unknown = compile_runtime_config(&format!(
        "schema_version = 1\ndefault_model = \"standard\"\n[agent.defaults.delegation]\nunknown = 1\n{}",
        model_table("standard", "acme", 4096)
    ));
    assert_eq!(unknown.state(), ConfigState::Invalid);
    assert!(
        unknown
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::UnknownField)
    );
}

#[test]
fn guardrail_modes_compile_without_a_core_off_variant() {
    let document = format!(
        r#"
schema_version = 1
default_model = "standard"

[agent.defaults.guardrails]
repeated_invocation = {{ mode = "off", threshold = 9 }}
consecutive_failures = {{ mode = "observe", threshold = 3 }}
{}
"#,
        model_table("standard", "acme", 4096)
    );
    let compilation = compile_runtime_config(&document);
    let active = compilation.active().expect("active configuration");
    assert!(active.guardrails().repeated_invocation.is_none());
    let failures = active
        .guardrails()
        .consecutive_failures
        .expect("failure guardrail");
    assert_eq!(failures.mode, ActiveGuardrailMode::Observe);
    assert_eq!(failures.threshold.get(), 3);

    let invalid = compile_runtime_config(&format!(
        r#"
schema_version = 1
default_model = "standard"

[agent.defaults.guardrails]
repeated_invocation = {{ mode = "enforce", threshold = 0 }}
{}
"#,
        model_table("standard", "acme", 4096)
    ));
    assert_eq!(invalid.state(), ConfigState::Invalid);
    assert!(
        invalid
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::InvalidLimit)
    );
}

#[test]
fn syntax_and_top_level_errors_fail_closed() {
    let missing = ConfigCompilation::missing();
    assert_eq!(missing.state(), ConfigState::Missing);
    assert!(missing.active().is_none());
    assert!(missing.issues().is_empty());

    let syntax = compile_runtime_config("schema_version = 1\nschema_version = 1");
    assert_eq!(syntax.state(), ConfigState::Invalid);
    assert!(syntax.active().is_none());
    assert_eq!(syntax.issues()[0].code(), ConfigIssueCode::InvalidSyntax);

    let unknown =
        compile_runtime_config("schema_version = 1\ndefault_model = \"model\"\nunknown = true");
    assert_eq!(unknown.state(), ConfigState::Invalid);
    assert_eq!(unknown.issues()[0].code(), ConfigIssueCode::UnknownField);

    let unsupported = compile_runtime_config("schema_version = 2\ndefault_model = \"model\"");
    assert_eq!(unsupported.state(), ConfigState::Invalid);
    assert_eq!(
        unsupported.issues()[0].code(),
        ConfigIssueCode::UnsupportedSchemaVersion
    );
}

#[test]
fn one_invalid_model_does_not_block_other_models() {
    let document = format!(
        r#"
schema_version = 1
default_model = "good"
{}

[models.bad]
protocol = "chat_completions"
provider = "acme"
endpoint = "https://api.example.test/v1"
model = "bad"
api_key = "secret"
context_window_tokens = 128000
max_output_tokens = 4096
typo_field = true
"#,
        model_table("good", "acme", 4096)
    );

    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Degraded);
    let active = compilation.active().expect("degraded active configuration");
    assert_eq!(active.models().len(), 1);
    assert!(
        active
            .models()
            .contains_key(&ModelKey::new("good").expect("key"))
    );
    assert!(
        compilation
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::UnknownField)
    );
}

#[test]
fn invalid_default_or_model_key_is_degraded_without_internal_id() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"invalid key\"\n{}",
        model_table("valid", "acme", 4096)
    );
    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Degraded);
    assert!(
        compilation
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::InvalidModelKey)
    );
    assert!(
        compilation
            .issues()
            .iter()
            .any(|issue| { issue.code() == ConfigIssueCode::DefaultModelUnavailable })
    );

    let invalid_table_key = compile_runtime_config(&format!(
        r#"schema_version = 1
default_model = "valid"
{}

[models."invalid key"]
protocol = "chat_completions"
provider = "acme"
endpoint = "https://api.example.test/v1"
model = "name"
api_key = "secret"
context_window_tokens = 100
max_output_tokens = 10
"#,
        model_table("valid", "acme", 4096)
    ));
    assert_eq!(invalid_table_key.state(), ConfigState::Degraded);
    assert!(invalid_table_key.projection().models.iter().any(|model| {
        model.model_key.is_none()
            && model
                .issues
                .iter()
                .any(|issue| issue.code() == ConfigIssueCode::InvalidModelKey)
    }));
}

#[test]
fn deepseek_thinking_rejects_sampling_parameters_without_invalidating_other_models() {
    let document = format!(
        r#"
schema_version = 1
default_model = "standard"

[agent.defaults.generation]
temperature = 0.7
top_p = 0.9
{}
{}

[models.deepseek.capabilities.reasoning]
enabled = true
"#,
        model_table("deepseek", "deepseek", 4096),
        model_table("standard", "acme", 4096)
    );

    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Degraded);
    let active = compilation.active().expect("degraded active configuration");
    assert_eq!(active.models().len(), 1);
    assert!(
        active
            .models()
            .contains_key(&ModelKey::new("standard").expect("key"))
    );
    assert!(compilation.issues().iter().any(|issue| {
        issue.code() == ConfigIssueCode::UnsupportedProfileCombination
            && issue
                .model_key()
                .is_some_and(|key| key.as_str() == "deepseek")
    }));
}

#[test]
fn catalog_matched_qwen_reasoning_and_image_capabilities_compile_as_ready() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"qwen\"\n{}",
        model_table("qwen", "dashscope", 4096).replace("model-name", "qwen3.8-max")
    );

    let compilation = compile_runtime_config_with_catalog(&document, &model_catalog());
    assert_eq!(compilation.state(), ConfigState::Ready);
    assert!(
        !compilation
            .issues()
            .iter()
            .any(|issue| { issue.code() == ConfigIssueCode::UnsupportedProfileCombination })
    );
    let model = compilation
        .active()
        .and_then(|active| active.model(&ModelKey::new("qwen").expect("key")))
        .expect("Qwen model");
    assert!(model.capabilities().image_input);
    let reasoning = model.capabilities().reasoning.as_ref().expect("reasoning");
    assert_eq!(reasoning.default_effort, Some(ReasoningEffortKey::XHigh));
    assert_eq!(reasoning.efforts.len(), 3);
}

#[test]
fn auxiliary_vision_projection_retains_selection_and_compiled_image_capability() {
    let document = format!(
        "schema_version = 1\ndefault_model = \"qwen\"\n\
         [agent.vision]\nmodel_key = \"qwen\"\ntimeout_ms = 60000\nmax_output_tokens = 4096\n{}",
        model_table("qwen", "dashscope", 4096).replace("model-name", "qwen3.8-max")
    );

    let compilation = compile_runtime_config_with_catalog(&document, &model_catalog());
    assert_eq!(compilation.state(), ConfigState::Ready);
    assert_eq!(
        compilation
            .projection()
            .auxiliary_vision_model
            .as_ref()
            .map(ModelKey::as_str),
        Some("qwen")
    );
    assert!(compilation.projection().models[0].supports_image_input);
    assert_eq!(
        compilation
            .active()
            .and_then(ResolvedConfig::vision)
            .map(|vision| vision.model_key.as_str()),
        Some("qwen")
    );
}

#[test]
fn effective_output_limit_uses_min_without_changing_context_or_budget() {
    for (agent_limit, model_limit, expected) in [
        (Some(2_048), 4_096, 2_048),
        (Some(8_192), 4_096, 4_096),
        (None, 4_096, 4_096),
    ] {
        let generation = agent_limit.map_or_else(String::new, |limit| {
            format!("[agent.defaults.generation]\nmax_output_tokens = {limit}\n")
        });
        let document = format!(
            "schema_version = 1\ndefault_model = \"standard\"\n{generation}{}",
            model_table("standard", "acme", model_limit)
        );
        let compilation = compile_runtime_config(&document);
        let active = compilation.active().expect("active configuration");
        let model = active.models().values().next().expect("model");
        assert_eq!(model.generation().max_output_tokens, Some(expected));
        assert_eq!(model.context_window_tokens(), 128_000);
        assert_eq!(active.budget(), &ExecutionBudget::default());
    }
}

#[test]
fn endpoint_accepts_remote_http_and_rejects_non_http_schemes() {
    let http_document = format!(
        "schema_version = 1\ndefault_model = \"standard\"\n{}",
        model_table("standard", "acme", 4096)
            .replace("https://api.example.test/v1", "http://api.example.test/v1")
    );
    let http_compilation = compile_runtime_config(&http_document);
    assert_eq!(http_compilation.state(), ConfigState::Ready);
    assert_eq!(
        http_compilation.projection().models[0].endpoint.as_deref(),
        Some("http://api.example.test/v1")
    );

    let ftp_document =
        http_document.replace("http://api.example.test/v1", "ftp://api.example.test/v1");
    let ftp_compilation = compile_runtime_config(&ftp_document);
    assert!(
        ftp_compilation
            .issues()
            .iter()
            .any(|issue| { issue.code() == ConfigIssueCode::InvalidEndpoint })
    );
    assert!(ftp_compilation.projection().models[0].endpoint.is_none());
}

#[test]
fn credential_and_unsafe_endpoint_never_enter_safe_debug_projection() {
    let document = format!(
        r#"
schema_version = 1
default_model = "unsafe"

[models.unsafe]
protocol = "chat_completions"
provider = "acme"
endpoint = "https://user:password-secret@example.test/v1?token=query-secret"
model = "name"
api_key = "{SECRET}"
context_window_tokens = 128000
max_output_tokens = 4096
"#
    );
    let compilation = compile_runtime_config(&document);
    let debug = format!("{:?} {:?}", compilation.projection(), compilation.issues());
    assert!(!debug.contains(SECRET));
    assert!(!debug.contains("password-secret"));
    assert!(!debug.contains("query-secret"));
    assert!(compilation.projection().models[0].endpoint.is_none());
    assert_eq!(
        format!("{:?}", ModelSecret::new(SECRET.to_owned())),
        "<redacted>"
    );
}

#[test]
fn invalid_global_policies_fail_closed_and_skip_models() {
    let document = format!(
        r#"
schema_version = 1
default_model = "standard"

[runtime.model_transport]
connect_timeout_ms = 2000
request_timeout_ms = 1000
{}
"#,
        model_table("standard", "acme", 4096)
    );
    let compilation = compile_runtime_config(&document);
    assert_eq!(compilation.state(), ConfigState::Invalid);
    assert!(compilation.active().is_none());
    assert!(compilation.projection().models.is_empty());
    assert_eq!(
        compilation.issues()[0].code(),
        ConfigIssueCode::InvalidLimit
    );
}

#[test]
fn missing_credential_and_bad_numeric_relationship_are_local() {
    let compilation = compile_runtime_config(
        r#"
schema_version = 1
default_model = "bad"

[models.bad]
protocol = "chat_completions"
provider = "acme"
endpoint = "https://api.example.test/v1"
model = "name"
context_window_tokens = 100
max_output_tokens = 101
"#,
    );
    assert_eq!(compilation.state(), ConfigState::Degraded);
    assert!(compilation.active().is_some());
    assert!(compilation.issues().iter().any(|issue| {
        issue.code() == ConfigIssueCode::MissingCredential
            && issue.model_key().is_some_and(|key| key.as_str() == "bad")
    }));
    assert!(
        compilation
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::InvalidLimit)
    );
}

#[test]
fn unsupported_protocol_and_invalid_provider_are_reported_per_model() {
    let compilation = compile_runtime_config(
        r#"
schema_version = 1
default_model = "bad"

[models.bad]
protocol = "responses"
provider = "invalid provider"
endpoint = "https://api.example.test/v1"
model = "name"
api_key = "secret"
context_window_tokens = 100
max_output_tokens = 10
"#,
    );

    assert_eq!(compilation.state(), ConfigState::Degraded);
    assert!(compilation.active().expect("active").models().is_empty());
    assert!(compilation.issues().iter().any(|issue| {
        issue.code() == ConfigIssueCode::UnsupportedProtocol
            && issue.model_key().is_some_and(|key| key.as_str() == "bad")
    }));
    assert!(compilation.issues().iter().any(|issue| {
        issue.code() == ConfigIssueCode::InvalidProvider
            && issue.model_key().is_some_and(|key| key.as_str() == "bad")
    }));
}

#[test]
fn invalid_retry_policy_is_a_top_level_policy_error() {
    let compilation = compile_runtime_config(
        r#"
schema_version = 1
default_model = "standard"

[runtime.model_retry]
retry_on = ["not_retryable"]
delays_ms = [100]
max_retry_after_ms = 1000
"#,
    );

    assert_eq!(compilation.state(), ConfigState::Invalid);
    assert!(compilation.active().is_none());
    assert!(
        compilation
            .issues()
            .iter()
            .any(|issue| issue.code() == ConfigIssueCode::InvalidPolicy)
    );
}
