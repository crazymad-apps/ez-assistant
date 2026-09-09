//! 全局策略验证；模型方言和参数覆盖在 managed_model/tests.rs 中检验。
use agent_core::{ActiveGuardrailMode, ExecutionBudget};
use assistant_runtime::*;
use std::time::Duration;

#[test]
fn optional_tables_keep_declared_defaults_without_a_model_catalog() {
    let result = compile_runtime_config("schema_version = 1\n");
    let active = result.active().unwrap();
    assert_eq!(result.state(), ConfigState::Ready);
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
    assert!(active.generation().max_output_tokens.is_none());
    assert_eq!(
        active
            .guardrails()
            .repeated_invocation
            .unwrap()
            .threshold
            .get(),
        4
    );
    assert_eq!(
        active
            .guardrails()
            .consecutive_failures
            .unwrap()
            .threshold
            .get(),
        5
    );
    let delegation = active.delegation();
    assert_eq!(delegation.max_tasks_per_run().get(), 8);
    assert_eq!(delegation.max_concurrent_tasks().get(), 4);
    assert_eq!(delegation.task_timeout(), Duration::from_secs(900));
    assert_eq!(delegation.max_steps().get(), 40);
    assert_eq!(delegation.max_tool_calls().get(), 100);
    assert_eq!(delegation.max_output_tokens().get(), 16_384);
    let mcp = active.mcp();
    assert_eq!(mcp.connect_timeout(), Duration::from_secs(15));
    assert_eq!(mcp.catalog_timeout(), Duration::from_secs(30));
    assert_eq!(mcp.request_timeout(), Duration::from_secs(120));
    assert_eq!(mcp.close_timeout(), Duration::from_secs(5));
    assert_eq!(mcp.max_concurrent_calls_per_server().get(), 8);
}

#[test]
fn missing_file_allows_initial_model_setup_with_global_defaults() {
    let result = ConfigCompilation::missing();
    assert_eq!(result.state(), ConfigState::Missing);
    assert!(result.active().is_some());
    assert!(result.projection().issues.is_empty());
    assert!(result.projection().schema_version.is_none());
}

#[test]
fn host_owned_tables_are_not_projected_and_global_policies_compile() {
    let result = compile_runtime_config(
        r#"
schema_version = 1
[speech.asr]
credential = "private-asr-secret"
[host_access]
password_hash = "private-hash"
[runtime.model_transport]
connect_timeout_ms = 2000
request_timeout_ms = 12000
[runtime.model_retry]
retry_on = ["connection", "timeout", "rate_limited", "unavailable"]
delays_ms = [10, 20]
max_retry_after_ms = 1000
[agent.defaults.generation]
temperature = 0.5
top_p = 0.9
max_output_tokens = 1234
stop = ["END"]
[agent.defaults.execution_limits]
max_steps = 5
max_tool_calls = 7
[agent.vision]
timeout_ms = 5000
max_output_tokens = 512
[mcp]
connect_timeout_ms = 1000
catalog_timeout_ms = 2000
request_timeout_ms = 600000
close_timeout_ms = 1000
max_concurrent_calls_per_server = 2
"#,
    );
    let active = result.active().unwrap();
    assert!(active.retry_policy().is_some());
    assert_eq!(
        active.transport().request_timeout(),
        Duration::from_secs(12)
    );
    assert_eq!(active.generation().max_output_tokens, Some(1234));
    assert_eq!(active.generation().temperature, Some(0.5));
    assert_eq!(active.generation().top_p, Some(0.9));
    assert_eq!(active.generation().stop, ["END"]);
    assert_eq!(active.budget().max_steps, Some(5));
    assert_eq!(active.budget().max_tool_calls, Some(7));
    assert_eq!(active.vision().unwrap().max_output_tokens, 512);
    assert_eq!(active.vision().unwrap().timeout, Duration::from_secs(5));
    assert_eq!(active.mcp().request_timeout(), Duration::from_secs(600));
    let projection = format!("{:?}", result.projection());
    assert!(!projection.contains("private-asr-secret"));
    assert!(!projection.contains("private-hash"));
}

#[test]
fn invalid_global_policy_rejects_the_whole_snapshot() {
    for section in [
        "[runtime.model_transport]\nconnect_timeout_ms=0",
        "[runtime.model_transport]\nconnect_timeout_ms=2000\nrequest_timeout_ms=1000",
        "[agent.defaults.generation]\nmax_output_tokens=0",
        "[agent.defaults.generation]\ntemperature=-0.1",
        "[agent.defaults.generation]\ntop_p=1.1",
        "[agent.defaults.generation]\nstop=[' ' ]",
        "[agent.defaults.execution_limits]\nmax_steps=0",
        "[agent.defaults.execution_limits]\nmax_tool_calls=0",
        "[agent.defaults.delegation]\nmax_concurrent_tasks=9",
        "[agent.defaults.delegation]\ntask_timeout_ms=0",
        "[agent.defaults.delegation]\nmax_output_tokens=0",
        "[agent.vision]\ntimeout_ms=0\nmax_output_tokens=1",
        "[mcp]\nconnect_timeout_ms=999",
        "[mcp]\ncatalog_timeout_ms=120001",
        "[mcp]\nrequest_timeout_ms=0",
        "[mcp]\nclose_timeout_ms=30001",
        "[mcp]\nmax_concurrent_calls_per_server=17",
        "[mcp]\nunknown_timeout=1",
        "[runtime.model_retry]\nretry_on=[]\ndelays_ms=[1]\nmax_retry_after_ms=1",
        "[runtime.model_retry]\nretry_on=['never']\ndelays_ms=[1]\nmax_retry_after_ms=1",
        "[runtime.model_retry]\nretry_on=['timeout']\ndelays_ms=[]\nmax_retry_after_ms=1",
        "[runtime.model_retry]\nretry_on=['timeout']\ndelays_ms=[1]",
    ] {
        let result = compile_runtime_config(&format!("schema_version=1\n{section}\n"));
        assert_eq!(result.state(), ConfigState::Invalid, "{section}");
        assert!(result.active().is_none(), "{section}");
        assert!(!result.projection().issues.is_empty(), "{section}");
    }
}

#[test]
fn guardrail_modes_map_explicitly_without_hidden_enforcement() {
    for (mode, expected) in [
        ("off", None),
        ("observe", Some(ActiveGuardrailMode::Observe)),
        ("enforce", Some(ActiveGuardrailMode::Enforce)),
    ] {
        let result = compile_runtime_config(&format!(
            "schema_version=1\n[agent.defaults.guardrails.repeated_invocation]\nmode='{mode}'\nthreshold=6\n"
        ));
        assert_eq!(
            result
                .active()
                .unwrap()
                .guardrails()
                .repeated_invocation
                .map(|guard| guard.mode),
            expected
        );
    }
}

#[test]
fn deprecated_model_fields_are_not_part_of_the_new_configuration_shape() {
    // 启动前 config_cleanup 删除这些键；纯编译器不保留旧字段、别名或旧模型执行路径。
    for document in [
        "schema_version=1\ndefault_model='old'",
        "schema_version=1\n[models.old]\napi_key='private-secret'",
        "schema_version=1\n[agent.vision]\nmodel_key='old'\ntimeout_ms=1\nmax_output_tokens=1",
    ] {
        let result = compile_runtime_config(document);
        assert_eq!(result.state(), ConfigState::Invalid);
        assert!(!format!("{:?}", result.projection()).contains("private-secret"));
    }
    for document in [
        "",
        "schema_version=2",
        "schema_version=1\nschema_version=1",
        "not valid TOML",
    ] {
        assert!(compile_runtime_config(document).active().is_none());
    }
}
