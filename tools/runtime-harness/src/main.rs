//! Version verification Runtime Harness.
//!
//! This binary is an explicit developer tool, not the product Assistant Runtime.

mod cli;
mod context;
mod debug;
mod demo_tool;
mod input;
mod journal;
mod output;
mod runtime;
mod scenario;

use std::sync::Arc;

use agent_context::{
    CompressionStrategy, ContextWindowEvaluator, RollingSummaryPolicy, RollingSummarySameModel,
};
use agent_core::{
    AgentEventStream, AgentExecution, CompletionFuture, ExecutionBudget, ExecutionOutcome,
    ExecutionSpec,
};
use agent_model::ModelService;
use agent_provider_openai_compatible::{
    BearerCredential, OpenAiCompatibleService, Profile, TransportTimeouts,
};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use futures_util::StreamExt;
use thiserror::Error;

use crate::{
    cli::Command,
    debug::RunDebug,
    demo_tool::LookupWeatherTool,
    input::InputAction,
    journal::JournalError,
    runtime::{HarnessRunId, HarnessRuntime},
};

#[derive(Debug, Error)]
pub(crate) enum HarnessError {
    #[error("CLI error: {0}")]
    Cli(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("tool registration error: {0}")]
    ToolRegistration(String),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("execution error: {0}")]
    Execution(String),
    #[error("scenario failed: {0}")]
    ScenarioFailed(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

impl HarnessError {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Cli(_) | Self::Config(_) => 2,
            _ => 1,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(error.exit_code());
    }
}

async fn run() -> Result<(), HarnessError> {
    match cli::parse_env()? {
        Command::Help => println!("{}", cli::HELP),
        Command::List => println!("{}", output::list_text()),
        Command::Verify { version } => {
            let summary = scenario::verify(version).await;
            println!("{}", output::format_verification(&summary));
            if !summary.is_success() {
                return Err(HarnessError::ScenarioFailed(format!(
                    "{} of {} `{version}` scenarios failed",
                    summary.failed(),
                    summary.results.len()
                )));
            }
        }
        Command::Chat {
            debug_url,
            debug_layer,
        } => {
            run_chat(debug_url, debug_layer).await?;
        }
    }
    Ok(())
}

#[derive(Debug)]
struct ChatConfig {
    api_key: String,
    base_url: String,
    model: String,
    context_window_tokens: u64,
    compaction_threshold_ratio: f64,
    summary_output_tokens: u32,
    minimum_recent_user_turns: u32,
    max_automatic_compactions: u32,
}

#[derive(Default)]
struct ChatConfigValues {
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    context_window_tokens: Option<String>,
    compaction_threshold_ratio: Option<String>,
    summary_output_tokens: Option<String>,
    minimum_recent_user_turns: Option<String>,
    max_automatic_compactions: Option<String>,
}

struct ChatResources {
    model: Arc<dyn ModelService>,
    context_window: Arc<ContextWindowEvaluator>,
    strategy: Arc<dyn CompressionStrategy>,
    tools: ToolSetSnapshot,
    endpoint: String,
    configured_model: String,
}

impl ChatResources {
    fn spec(&self, debug: Option<&RunDebug>) -> ExecutionSpec {
        let model = debug.map_or_else(
            || Arc::clone(&self.model),
            |debug| debug.observe_model(Arc::clone(&self.model)),
        );
        ExecutionSpec {
            instructions: vec![
                "You are running inside the ez-assistant Runtime Harness.".to_owned(),
                "For weather questions, call lookup_weather before answering and clearly state \
                 that its fixed result is demo data."
                    .to_owned(),
            ],
            model,
            context_window: Arc::clone(&self.context_window),
            tools: self.tools.clone(),
            budget: ExecutionBudget {
                max_steps: Some(8),
                max_tool_calls: Some(8),
            },
            guardrails: None,
        }
    }
}

struct ActiveRun {
    run_id: HarnessRunId,
    events: AgentEventStream,
    completion: CompletionFuture,
    outcome: Option<ExecutionOutcome>,
    events_closed: bool,
    debug: Option<RunDebug>,
}

impl ActiveRun {
    fn new(run_id: HarnessRunId, execution: AgentExecution, debug: Option<RunDebug>) -> Self {
        Self {
            run_id,
            events: execution.events,
            completion: execution.completion,
            outcome: None,
            events_closed: false,
            debug,
        }
    }

    fn is_ready_to_finish(&self) -> bool {
        self.outcome.is_some() && self.events_closed
    }
}

enum ChatSignal {
    Input(Option<input::InputResult>),
    Event(Option<agent_core::AgentEvent>),
    Outcome(ExecutionOutcome),
}

async fn run_chat(
    debug_url: Option<String>,
    debug_layer: cli::DebugLayerSelection,
) -> Result<(), HarnessError> {
    dotenvy::dotenv().ok();
    let debug_url = debug_url.or_else(|| {
        std::env::var("DEBUG_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
    });
    let config = load_chat_config()?;
    let endpoint = sanitized_url(&config.base_url);
    let service = OpenAiCompatibleService::new(
        config.base_url,
        BearerCredential::new(config.api_key),
        config.model.clone(),
        config.context_window_tokens,
        Profile::deepseek(),
        TransportTimeouts::default(),
    )
    .map_err(|error| HarnessError::Provider(error.to_string()))?;
    let resources = ChatResources {
        model: Arc::new(service),
        context_window: Arc::new(
            ContextWindowEvaluator::new(config.compaction_threshold_ratio)
                .map_err(|error| HarnessError::Config(error.to_string()))?,
        ),
        strategy: Arc::new(RollingSummarySameModel::new(
            RollingSummaryPolicy::new(
                config.summary_output_tokens,
                config.minimum_recent_user_turns,
            )
            .map_err(|error| HarnessError::Config(error.to_string()))?,
        )),
        tools: demo_tools()?,
        endpoint,
        configured_model: config.model.clone(),
    };
    let mut runtime =
        HarnessRuntime::new_with_max_automatic_compactions(config.max_automatic_compactions)?;
    let mut commands = input::spawn_stdin()?;
    let mut active: Option<ActiveRun> = None;
    let mut quit_requested = false;
    let mut accepting_commands = true;
    let mut input_error = None;

    println!(
        "DeepSeek Runtime Harness（模型：{}）。输入消息，或使用 \
         /state /compact /reset /cancel /quit。",
        config.model
    );
    if let Some(url) = debug_url.as_deref() {
        println!(
            "debug 已启用：{}（layer={debug_layer}）",
            sanitized_url(url)
        );
    }

    loop {
        if active.as_ref().is_some_and(ActiveRun::is_ready_to_finish) {
            let Some(mut finished) = active.take() else {
                return Err(HarnessError::Execution(
                    "ready run disappeared before finalization".to_owned(),
                ));
            };
            let Some(outcome) = finished.outcome.take() else {
                return Err(HarnessError::Execution(
                    "ready run has no completion outcome".to_owned(),
                ));
            };
            runtime.finish_run(&finished.run_id, outcome.clone())?;
            let snapshot = runtime.snapshot()?;
            if let Some(debug) = &finished.debug {
                debug.post_run_finished(&snapshot, &outcome);
            }
            println!(
                "run {}: {}",
                finished.run_id,
                output::format_outcome(&outcome)
            );
            if quit_requested {
                if let Some(error) = input_error {
                    return Err(HarnessError::Io(error));
                }
                break;
            }
            if let ExecutionOutcome::CompactionRequired { reason, step } = outcome {
                match start_continuation_run(
                    &mut runtime,
                    &resources,
                    finished.run_id,
                    reason,
                    step,
                    finished.debug.as_ref(),
                    debug_url.as_deref(),
                    debug_layer,
                )
                .await
                {
                    Ok(next) => {
                        active = Some(next);
                        continue;
                    }
                    Err(error) => {
                        eprintln!("automatic continuation failed: {error}");
                    }
                }
            }
            if runtime.take_queued_user_compaction() {
                match perform_user_compaction(
                    &mut runtime,
                    &resources,
                    debug_url.as_deref(),
                    debug_layer,
                )
                .await
                {
                    Ok(outcome) => {
                        println!(
                            "queued /compact: {}",
                            output::format_compaction_outcome(&outcome)
                        );
                    }
                    Err(error) => eprintln!("queued /compact failed: {error}"),
                }
            }
            continue;
        }

        let Some(running) = active.as_mut() else {
            let Some(result) = commands.recv().await else {
                break;
            };
            let command = result?;
            match input::action_for(command, false) {
                InputAction::Start(text) => {
                    match start_initial_run(
                        &mut runtime,
                        &resources,
                        &text,
                        debug_url.as_deref(),
                        debug_layer,
                    )
                    .await
                    {
                        Ok(run) => active = Some(run),
                        Err(error) => eprintln!("run preparation failed: {error}"),
                    }
                }
                InputAction::ShowState => {
                    print!("{}", output::format_state(&runtime.snapshot()?));
                }
                InputAction::Compact => {
                    match perform_user_compaction(
                        &mut runtime,
                        &resources,
                        debug_url.as_deref(),
                        debug_layer,
                    )
                    .await
                    {
                        Ok(outcome) => {
                            println!("/compact: {}", output::format_compaction_outcome(&outcome));
                        }
                        Err(error) => eprintln!("/compact failed: {error}"),
                    }
                }
                InputAction::Reset => {
                    runtime.reset()?;
                    println!("session journal reset");
                }
                InputAction::Quit => break,
                InputAction::Reject(message) => eprintln!("{message}"),
                InputAction::QueueCompaction | InputAction::Cancel | InputAction::CancelAndQuit => {
                    return Err(HarnessError::Execution(
                        "invalid idle command disposition".to_owned(),
                    ));
                }
            }
            continue;
        };

        let signal = tokio::select! {
            result = commands.recv(), if accepting_commands => ChatSignal::Input(result),
            event = running.events.next(), if !running.events_closed => ChatSignal::Event(event),
            outcome = &mut running.completion, if running.outcome.is_none() => {
                ChatSignal::Outcome(outcome)
            }
        };

        match signal {
            ChatSignal::Input(Some(Ok(command))) => match input::action_for(command, true) {
                InputAction::ShowState => {
                    print!("{}", output::format_state(&runtime.snapshot()?));
                }
                InputAction::QueueCompaction => {
                    runtime.queue_user_compaction();
                    if let Some(debug) = &running.debug {
                        debug.post_compaction_queued();
                    }
                    println!("/compact queued; it will run after the active task chain");
                }
                InputAction::Cancel => {
                    runtime.cancel_active()?;
                    if let Some(debug) = &running.debug {
                        debug.post_cancel_requested();
                    }
                    println!("cancel requested");
                }
                InputAction::CancelAndQuit => {
                    runtime.cancel_active()?;
                    if let Some(debug) = &running.debug {
                        debug.post_cancel_requested();
                    }
                    quit_requested = true;
                    accepting_commands = false;
                    println!("cancel requested; waiting for run cleanup before exit");
                }
                InputAction::Reject(message) => eprintln!("{message}"),
                InputAction::Start(_)
                | InputAction::Compact
                | InputAction::Reset
                | InputAction::Quit => {
                    return Err(HarnessError::Execution(
                        "invalid active command disposition".to_owned(),
                    ));
                }
            },
            ChatSignal::Input(Some(Err(error))) => {
                runtime.cancel_active()?;
                if let Some(debug) = &running.debug {
                    debug.post_cancel_requested();
                }
                quit_requested = true;
                accepting_commands = false;
                input_error = Some(error);
            }
            ChatSignal::Input(None) => {
                runtime.cancel_active()?;
                if let Some(debug) = &running.debug {
                    debug.post_cancel_requested();
                }
                quit_requested = true;
                accepting_commands = false;
            }
            ChatSignal::Event(Some(event)) => {
                runtime.observe_event(&running.run_id, &event)?;
                if let Some(debug) = &running.debug {
                    debug.post_agent(&event);
                }
                println!("{}", output::format_event(&event));
            }
            ChatSignal::Event(None) => {
                running.events_closed = true;
            }
            ChatSignal::Outcome(outcome) => {
                running.outcome = Some(outcome);
            }
        }
    }
    Ok(())
}

async fn start_initial_run(
    runtime: &mut HarnessRuntime,
    resources: &ChatResources,
    text: &str,
    debug_url: Option<&str>,
    debug_layer: cli::DebugLayerSelection,
) -> Result<ActiveRun, HarnessError> {
    let expected_run_id = runtime.next_run_id();
    let snapshot = runtime.snapshot()?;
    let debug = RunDebug::for_run(
        debug_url,
        debug_layer,
        &snapshot,
        &expected_run_id,
        runtime.correlation_id(&expected_run_id),
        &resources.endpoint,
        &resources.configured_model,
    );
    let prepared = runtime
        .prepare_context_run(
            text,
            Arc::new(resources.spec(debug.as_ref())),
            Arc::clone(&resources.strategy),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    if prepared.run_id != expected_run_id {
        return Err(HarnessError::Execution(
            "prepared initial run id changed during context orchestration".to_owned(),
        ));
    }
    if !matches!(prepared.kind, context::HarnessRunContextKind::Initial) {
        return Err(HarnessError::Execution(
            "initial context preparation returned a continuation".to_owned(),
        ));
    }
    if let Some(debug) = &debug {
        if let Some(user) = &prepared.user {
            debug.post_user_message(user);
        }
        if let Some(evaluation) = &prepared.preflight {
            debug.post_context_preflight(evaluation);
        }
        if let Some(report) = &prepared.compaction {
            debug.post_compaction_report("compacted", report, runtime.snapshot()?.checkpoint_count);
        }
    }
    let (run_id, execution) = runtime.start_prepared(resources.spec(debug.as_ref()), prepared)?;
    if let Some(debug) = &debug {
        debug.post_run_started(&runtime.snapshot()?);
    }
    println!("run {run_id}: started");
    Ok(ActiveRun::new(run_id, execution, debug))
}

#[allow(clippy::too_many_arguments)]
async fn start_continuation_run(
    runtime: &mut HarnessRuntime,
    resources: &ChatResources,
    previous_run_id: HarnessRunId,
    reason: agent_core::CompactionReason,
    step: u32,
    previous_debug: Option<&RunDebug>,
    debug_url: Option<&str>,
    debug_layer: cli::DebugLayerSelection,
) -> Result<ActiveRun, HarnessError> {
    let prepared = runtime
        .prepare_continuation(
            previous_run_id.clone(),
            reason,
            step,
            Arc::new(resources.spec(previous_debug)),
            Arc::clone(&resources.strategy),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    if !matches!(
        &prepared.kind,
        context::HarnessRunContextKind::Continuation {
            previous_run_id: actual
        } if actual == &previous_run_id
    ) {
        return Err(HarnessError::Execution(
            "continuation context did not reference the completed run".to_owned(),
        ));
    }
    if let (Some(debug), Some(report)) = (previous_debug, prepared.compaction.as_ref()) {
        if let Some(evaluation) = &report.trigger {
            debug.post_context_preflight(evaluation);
        }
        debug.post_compaction_report("compacted", report, runtime.snapshot()?.checkpoint_count);
    }
    let run_id = prepared.run_id.clone();
    let snapshot = runtime.snapshot()?;
    let debug = RunDebug::for_run(
        debug_url,
        debug_layer,
        &snapshot,
        &run_id,
        runtime.correlation_id(&run_id),
        &resources.endpoint,
        &resources.configured_model,
    );
    if let Some(debug) = &debug {
        debug.post_continuation_started(&previous_run_id, &run_id);
    }
    let (run_id, execution) = runtime.start_prepared(resources.spec(debug.as_ref()), prepared)?;
    if let Some(debug) = &debug {
        debug.post_run_started(&runtime.snapshot()?);
    }
    println!("run {run_id}: continuation of {previous_run_id}");
    Ok(ActiveRun::new(run_id, execution, debug))
}

async fn perform_user_compaction(
    runtime: &mut HarnessRuntime,
    resources: &ChatResources,
    debug_url: Option<&str>,
    debug_layer: cli::DebugLayerSelection,
) -> Result<context::HarnessCompactionOutcome, HarnessError> {
    let snapshot = runtime.snapshot()?;
    let debug = RunDebug::for_context_operation(
        debug_url,
        debug_layer,
        &snapshot,
        &resources.endpoint,
        &resources.configured_model,
    );
    let outcome = runtime
        .compact_user_context(
            Arc::new(resources.spec(debug.as_ref())),
            Arc::clone(&resources.strategy),
            tokio_util::sync::CancellationToken::new(),
        )
        .await?;
    if let Some(debug) = &debug {
        debug.post_user_compaction_outcome(&outcome, runtime.snapshot()?.checkpoint_count);
    }
    Ok(outcome)
}

fn load_chat_config() -> Result<ChatConfig, HarnessError> {
    chat_config_from_values(ChatConfigValues {
        api_key: std::env::var("DEEPSEEK_API_KEY").ok(),
        base_url: std::env::var("DEEPSEEK_BASE_URL").ok(),
        model: std::env::var("DEEPSEEK_MODEL").ok(),
        context_window_tokens: std::env::var("DEEPSEEK_CONTEXT_WINDOW_TOKENS").ok(),
        compaction_threshold_ratio: std::env::var("HARNESS_COMPACTION_THRESHOLD_RATIO").ok(),
        summary_output_tokens: std::env::var("HARNESS_SUMMARY_OUTPUT_TOKENS").ok(),
        minimum_recent_user_turns: std::env::var("HARNESS_MINIMUM_RECENT_USER_TURNS").ok(),
        max_automatic_compactions: std::env::var("HARNESS_MAX_AUTOMATIC_COMPACTIONS").ok(),
    })
}

fn chat_config_from_values(values: ChatConfigValues) -> Result<ChatConfig, HarnessError> {
    let ChatConfigValues {
        api_key,
        base_url,
        model,
        context_window_tokens,
        compaction_threshold_ratio,
        summary_output_tokens,
        minimum_recent_user_turns,
        max_automatic_compactions,
    } = values;
    let api_key = api_key
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            HarnessError::Config(
                "missing DEEPSEEK_API_KEY; configure it in the repository .env".to_owned(),
            )
        })?;
    let base_url = base_url
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "https://api.deepseek.com".to_owned());
    let model = model
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "deepseek-v4-flash".to_owned());
    let context_window_tokens = context_window_tokens
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "128000".to_owned())
        .parse::<u64>()
        .map_err(|_| {
            HarnessError::Config(
                "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be a positive integer".to_owned(),
            )
        })?;
    if context_window_tokens == 0 {
        return Err(HarnessError::Config(
            "DEEPSEEK_CONTEXT_WINDOW_TOKENS must be greater than zero".to_owned(),
        ));
    }
    let compaction_threshold_ratio = parse_ratio(
        "HARNESS_COMPACTION_THRESHOLD_RATIO",
        compaction_threshold_ratio,
        0.8,
    )?;
    let summary_output_tokens = parse_positive_u32(
        "HARNESS_SUMMARY_OUTPUT_TOKENS",
        summary_output_tokens,
        1_024,
    )?;
    let minimum_recent_user_turns = parse_u32(
        "HARNESS_MINIMUM_RECENT_USER_TURNS",
        minimum_recent_user_turns,
        1,
    )?;
    let max_automatic_compactions = parse_positive_u32(
        "HARNESS_MAX_AUTOMATIC_COMPACTIONS",
        max_automatic_compactions,
        2,
    )?;
    Ok(ChatConfig {
        api_key,
        base_url,
        model,
        context_window_tokens,
        compaction_threshold_ratio,
        summary_output_tokens,
        minimum_recent_user_turns,
        max_automatic_compactions,
    })
}

fn parse_ratio(name: &str, value: Option<String>, default: f64) -> Result<f64, HarnessError> {
    let ratio = value
        .filter(|value| !value.trim().is_empty())
        .map_or(Ok(default), |value| {
            value
                .parse::<f64>()
                .map_err(|_| HarnessError::Config(format!("{name} must be a number in (0, 1]")))
        })?;
    if !ratio.is_finite() || ratio <= 0.0 || ratio > 1.0 {
        return Err(HarnessError::Config(format!(
            "{name} must be a finite number in (0, 1]"
        )));
    }
    Ok(ratio)
}

fn parse_u32(name: &str, value: Option<String>, default: u32) -> Result<u32, HarnessError> {
    value
        .filter(|value| !value.trim().is_empty())
        .map_or(Ok(default), |value| {
            value
                .parse::<u32>()
                .map_err(|_| HarnessError::Config(format!("{name} must be a non-negative integer")))
        })
}

fn parse_positive_u32(
    name: &str,
    value: Option<String>,
    default: u32,
) -> Result<u32, HarnessError> {
    let parsed = parse_u32(name, value, default)?;
    if parsed == 0 {
        return Err(HarnessError::Config(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(parsed)
}

fn demo_tools() -> Result<ToolSetSnapshot, HarnessError> {
    let mut registry = ToolRegistry::new();
    registry
        .register(LookupWeatherTool)
        .map_err(|error| HarnessError::ToolRegistration(error.to_string()))?;
    Ok(registry.snapshot())
}

fn sanitized_url(value: &str) -> String {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return "<invalid URL>".to_owned();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_and_config_errors_use_exit_code_two() {
        assert_eq!(HarnessError::Cli("bad".to_owned()).exit_code(), 2);
        assert_eq!(HarnessError::Config("bad".to_owned()).exit_code(), 2);
        assert_eq!(HarnessError::Execution("bad".to_owned()).exit_code(), 1);
    }

    #[test]
    fn chat_config_requires_a_key_without_exposing_values_and_uses_defaults() {
        let error = match chat_config_from_values(ChatConfigValues::default()) {
            Ok(_) => panic!("key must be required"),
            Err(error) => error,
        };
        assert_eq!(error.exit_code(), 2);
        assert!(!error.to_string().contains("secret"));

        let config = chat_config_from_values(ChatConfigValues {
            api_key: Some("secret".to_owned()),
            base_url: Some(" ".to_owned()),
            model: Some(String::new()),
            ..ChatConfigValues::default()
        })
        .expect("config");
        assert_eq!(config.base_url, "https://api.deepseek.com");
        assert_eq!(config.model, "deepseek-v4-flash");
        assert_eq!(config.context_window_tokens, 128_000);
        assert_eq!(config.compaction_threshold_ratio, 0.8);
        assert_eq!(config.summary_output_tokens, 1_024);
        assert_eq!(config.minimum_recent_user_turns, 1);
        assert_eq!(config.max_automatic_compactions, 2);
    }

    #[test]
    fn chat_config_rejects_invalid_context_window() {
        for value in ["0", "not-a-number"] {
            let error = match chat_config_from_values(ChatConfigValues {
                api_key: Some("secret".to_owned()),
                context_window_tokens: Some(value.to_owned()),
                ..ChatConfigValues::default()
            }) {
                Ok(_) => panic!("invalid context window must fail"),
                Err(error) => error,
            };
            assert!(
                error.to_string().contains("DEEPSEEK_CONTEXT_WINDOW_TOKENS"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn chat_config_rejects_invalid_compaction_values() {
        for (ratio, summary, recent, automatic, field) in [
            (Some("0"), None, None, None, "THRESHOLD"),
            (Some("NaN"), None, None, None, "THRESHOLD"),
            (None, Some("0"), None, None, "SUMMARY_OUTPUT"),
            (None, None, Some("invalid"), None, "MINIMUM_RECENT"),
            (None, None, None, Some("0"), "MAX_AUTOMATIC"),
        ] {
            let error = chat_config_from_values(ChatConfigValues {
                api_key: Some("secret".to_owned()),
                compaction_threshold_ratio: ratio.map(str::to_owned),
                summary_output_tokens: summary.map(str::to_owned),
                minimum_recent_user_turns: recent.map(str::to_owned),
                max_automatic_compactions: automatic.map(str::to_owned),
                ..ChatConfigValues::default()
            })
            .expect_err("invalid compaction config must fail");
            assert!(
                error.to_string().contains(field),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn debug_url_projection_removes_embedded_credentials() {
        let projected =
            sanitized_url("https://user:password@api.example.com/v1?api_key=secret#token");
        assert_eq!(projected, "https://api.example.com/v1");
        for secret in ["user", "password", "api_key", "secret", "token"] {
            assert!(!projected.contains(secret));
        }
        assert_eq!(sanitized_url("not a URL"), "<invalid URL>");
    }
}
