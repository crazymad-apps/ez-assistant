use super::domain::{ModelProfile, ModelSecret};
use super::*;
use agent_core::ExecutionBudget;
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
    assert_eq!(
        ModelProfile::for_provider(model.provider()),
        ModelProfile::DeepSeek
    );
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
    let model = active.models().values().next().expect("compiled model");
    assert_eq!(model.provider().as_str(), "302-ai");
    assert_eq!(
        ModelProfile::for_provider(model.provider()),
        ModelProfile::Standard
    );
    assert_eq!(model.generation().max_output_tokens, Some(4_096));
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
fn profile_combination_only_invalidates_deepseek_models() {
    let document = format!(
        r#"
schema_version = 1
default_model = "standard"

[agent.defaults.generation]
temperature = 0.7
top_p = 0.9
{}
{}
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
