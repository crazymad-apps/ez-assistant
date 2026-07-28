//! Version verification Runtime Harness.
//!
//! This binary is an explicit developer tool, not the product Assistant Runtime.

mod cli;
mod debug;
mod demo_tool;
mod input;
mod journal;
mod output;
mod runtime;
mod scenario;

use std::sync::Arc;

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

struct ChatConfig {
    api_key: String,
    base_url: String,
    model: String,
}

struct ChatResources {
    model: Arc<dyn ModelService>,
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
            tools: self.tools.clone(),
            budget: ExecutionBudget {
                max_steps: Some(8),
                max_tool_calls: Some(8),
            },
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
        Profile::deepseek(),
        TransportTimeouts::default(),
    )
    .map_err(|error| HarnessError::Provider(error.to_string()))?;
    let resources = ChatResources {
        model: Arc::new(service),
        tools: demo_tools()?,
        endpoint,
        configured_model: config.model.clone(),
    };
    let mut runtime = HarnessRuntime::new()?;
    let mut commands = input::spawn_stdin()?;
    let mut active: Option<ActiveRun> = None;
    let mut quit_requested = false;
    let mut accepting_commands = true;
    let mut input_error = None;

    println!(
        "DeepSeek Runtime Harness（模型：{}）。输入消息，或使用 \
         /state /reset /cancel /quit。",
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
            continue;
        }

        let Some(running) = active.as_mut() else {
            let Some(result) = commands.recv().await else {
                break;
            };
            let command = result?;
            match input::action_for(command, false) {
                InputAction::Start(text) => {
                    let prepared = runtime.prepare_run(&text)?;
                    let run_id = prepared.run_id.clone();
                    let prepared_snapshot = runtime.snapshot()?;
                    let correlation_id = runtime.correlation_id(&run_id);
                    let debug = RunDebug::for_run(
                        debug_url.as_deref(),
                        debug_layer,
                        &prepared_snapshot,
                        &run_id,
                        correlation_id,
                        &resources.endpoint,
                        &resources.configured_model,
                    );
                    if let Some(debug) = &debug {
                        debug.post_user_message(&prepared.input.user_input);
                    }
                    let (run_id, execution) =
                        runtime.start_prepared(resources.spec(debug.as_ref()), prepared)?;
                    let running_snapshot = runtime.snapshot()?;
                    if let Some(debug) = &debug {
                        debug.post_run_started(&running_snapshot);
                    }
                    println!("run {run_id}: started");
                    active = Some(ActiveRun::new(run_id, execution, debug));
                }
                InputAction::ShowState => {
                    print!("{}", output::format_state(&runtime.snapshot()?));
                }
                InputAction::Reset => {
                    runtime.reset()?;
                    println!("session journal reset");
                }
                InputAction::Quit => break,
                InputAction::Reject(message) => eprintln!("{message}"),
                InputAction::Cancel | InputAction::CancelAndQuit => {
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
                InputAction::Start(_) | InputAction::Reset | InputAction::Quit => {
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

fn load_chat_config() -> Result<ChatConfig, HarnessError> {
    chat_config_from_values(
        std::env::var("DEEPSEEK_API_KEY").ok(),
        std::env::var("DEEPSEEK_BASE_URL").ok(),
        std::env::var("DEEPSEEK_MODEL").ok(),
    )
}

fn chat_config_from_values(
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<ChatConfig, HarnessError> {
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
    Ok(ChatConfig {
        api_key,
        base_url,
        model,
    })
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
        let error = match chat_config_from_values(None, None, None) {
            Ok(_) => panic!("key must be required"),
            Err(error) => error,
        };
        assert_eq!(error.exit_code(), 2);
        assert!(!error.to_string().contains("secret"));

        let config = chat_config_from_values(
            Some("secret".to_owned()),
            Some(" ".to_owned()),
            Some(String::new()),
        )
        .expect("config");
        assert_eq!(config.base_url, "https://api.deepseek.com");
        assert_eq!(config.model, "deepseek-v4-flash");
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
