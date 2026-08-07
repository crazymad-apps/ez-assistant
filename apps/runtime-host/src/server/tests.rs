use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_model::{ModelCapabilities, ModelService, SystemPromptSnapshot};
use agent_sdk::AllowAllAuthorizer;
use agent_testkit::{ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, ProviderId, TextPart,
    ToolCall, ToolCallId, ToolName,
};
use assistant_protocol::{
    CancelRunRequest, CreateSessionRequest, GetRunRequest, PROTOCOL_VERSION, RunStatus,
    RuntimeCommand, RuntimeCommandResult, ShutdownRuntimeRequest, StartRunRequest,
};
use assistant_runtime::{
    AssistantRuntime, ConfigSourceFuture, ConfigSourceLoad, ModelServiceFactory,
    ModelServiceFactoryError, ModelServiceFactoryRequest, RuntimeConfig, RuntimeConfigSource,
    SystemPromptFactory, SystemPromptFactoryError,
};
use serde_json::json;
use tempfile::tempdir;
use tokio::{io::AsyncWriteExt, net::UnixStream, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{RuntimeServer, ServerError};
use crate::{
    endpoint::{EndpointError, OwnedEndpoint},
    wire::{
        ClientFrame, HostCommand, HostCommandResult, MAX_FRAME_BYTES, ServerFrame, read_frame,
        write_frame,
    },
};

const TEST_CONFIG: &str = r#"
schema_version = 1
default_model = "fixture"

[models.fixture]
protocol = "chat_completions"
provider = "fixture"
endpoint = "https://api.example.test/v1"
model = "fixture-model"
api_key = "unique-test-secret-9f1ca2"
context_window_tokens = 8192
max_output_tokens = 4096
"#;

struct MutableConfigSource {
    document: Mutex<String>,
}

impl MutableConfigSource {
    fn new(document: impl Into<String>) -> Self {
        Self {
            document: Mutex::new(document.into()),
        }
    }

    fn replace(&self, document: impl Into<String>) {
        *self.document.lock().expect("source lock") = document.into();
    }
}

impl RuntimeConfigSource for MutableConfigSource {
    fn display_path(&self) -> Option<String> {
        Some("/private/runtime/config.toml".to_owned())
    }

    fn load(&self) -> ConfigSourceFuture<'_> {
        let document = self.document.lock().expect("source lock").clone();
        Box::pin(std::future::ready(ConfigSourceLoad::Document(document)))
    }
}

struct StaticModelFactory {
    model: Arc<dyn ModelService>,
}

impl ModelServiceFactory for StaticModelFactory {
    fn create_model(
        &self,
        _request: ModelServiceFactoryRequest<'_>,
    ) -> Result<Arc<dyn ModelService>, ModelServiceFactoryError> {
        Ok(self.model.clone())
    }
}

struct StaticSystemPromptFactory;

impl SystemPromptFactory for StaticSystemPromptFactory {
    fn create_system_prompt(&self) -> Result<SystemPromptSnapshot, SystemPromptFactoryError> {
        Ok(SystemPromptSnapshot::new(vec![
            "Host test agent".to_owned(),
        ]))
    }
}

fn capabilities(has_tools: bool) -> ModelCapabilities {
    ModelCapabilities {
        reasoning: false,
        tool_calls: has_tools,
        streaming: true,
    }
}

async fn runtime(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> Arc<AssistantRuntime> {
    runtime_with_source(model, tools).await.0
}

async fn runtime_with_source(
    model: Arc<dyn ModelService>,
    tools: ToolSetSnapshot,
) -> (Arc<AssistantRuntime>, Arc<MutableConfigSource>) {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG));
    let runtime = Arc::new(AssistantRuntime::new(
        RuntimeConfig::new(NonZeroUsize::new(64).expect("capacity")),
        source.clone(),
        Arc::new(StaticModelFactory { model }),
        Arc::new(StaticSystemPromptFactory),
        tools,
        Arc::new(AllowAllAuthorizer),
    ));
    runtime
        .reload_config(assistant_protocol::ReloadConfigRequest::default())
        .await
        .expect("load config");
    (runtime, source)
}

async fn empty_runtime() -> Arc<AssistantRuntime> {
    runtime(
        Arc::new(ScriptedModelService::new(capabilities(false), 8_192, [])),
        ToolSetSnapshot::default(),
    )
    .await
}

fn tool_message() -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("assistant-tools").expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::ToolCall(ToolCall {
            id: ToolCallId::new("call-1").expect("call id"),
            name: ToolName::new("slow_tool").expect("tool name"),
            arguments: json!({"value": "hello"}),
        })],
        finish_reason: FinishReason::ToolCalls,
        usage: None,
    }
}

fn text_message(message_id: &str, text: &str) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new(message_id).expect("message id"),
        model: ModelIdentity::new(
            ProviderId::new("fixture").expect("provider"),
            "fixture-model",
        ),
        parts: vec![AssistantPart::Text(TextPart {
            id: agent_types::PartId::new(format!("{message_id}-text")).expect("part id"),
            text: text.to_owned(),
        })],
        finish_reason: FinishReason::Stop,
        usage: None,
    }
}

async fn connect(path: &std::path::Path) -> UnixStream {
    let mut stream = UnixStream::connect(path).await.expect("connect");
    write_frame(
        &mut stream,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "integration-test".to_owned(),
        },
    )
    .await
    .expect("hello");
    assert!(matches!(
        read_frame::<_, ServerFrame>(&mut stream)
            .await
            .expect("ack"),
        Some(ServerFrame::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            ..
        })
    ));
    stream
}

async fn request(
    stream: &mut UnixStream,
    request_id: &str,
    command: HostCommand,
) -> HostCommandResult {
    write_frame(
        stream,
        &ClientFrame::Request {
            request_id: request_id.to_owned(),
            command,
        },
    )
    .await
    .expect("request");
    loop {
        match read_frame::<_, ServerFrame>(stream)
            .await
            .expect("response frame")
            .expect("connection remains open")
        {
            ServerFrame::Response {
                request_id: actual,
                result,
            } if actual == request_id => return result,
            ServerFrame::Error {
                request_id: actual,
                error,
            } if actual == request_id => panic!("request failed: {error:?}"),
            ServerFrame::Event { .. } => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn request_error(
    stream: &mut UnixStream,
    request_id: &str,
    command: HostCommand,
) -> assistant_protocol::RuntimeErrorInfo {
    write_frame(
        stream,
        &ClientFrame::Request {
            request_id: request_id.to_owned(),
            command,
        },
    )
    .await
    .expect("request");
    loop {
        match read_frame::<_, ServerFrame>(stream)
            .await
            .expect("response frame")
            .expect("connection remains open")
        {
            ServerFrame::Error {
                request_id: actual,
                error,
            } if actual == request_id => return error,
            ServerFrame::Event { .. } => {}
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}

async fn start_server(
    path: std::path::PathBuf,
    runtime: Arc<AssistantRuntime>,
) -> (CancellationToken, JoinHandle<Result<(), ServerError>>) {
    let endpoint = OwnedEndpoint::bind(path).expect("bind");
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        RuntimeServer::new(endpoint, runtime)
            .serve_until(task_shutdown)
            .await
    });
    (shutdown, task)
}

#[tokio::test]
async fn handshake_commands_single_instance_and_controlled_stop_work_end_to_end() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("runtime").join("runtime.sock");
    let runtime = empty_runtime().await;
    let (_shutdown, server) = start_server(path.clone(), runtime).await;

    let mut invalid = UnixStream::connect(&path).await.expect("connect invalid");
    write_frame(
        &mut invalid,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION + 1,
            client_name: "wrong-version".to_owned(),
        },
    )
    .await
    .expect("invalid hello");
    assert!(matches!(
        read_frame::<_, ServerFrame>(&mut invalid)
            .await
            .expect("rejection"),
        Some(ServerFrame::Error { request_id, .. }) if request_id == "handshake"
    ));
    drop(invalid);

    let mut client = connect(&path).await;
    let HostCommandResult::Runtime(RuntimeCommandResult::CreateSession(created)) = request(
        &mut client,
        "create",
        HostCommand::Runtime(RuntimeCommand::CreateSession(CreateSessionRequest {
            title: Some("Host session".to_owned()),
            model_key: None,
        })),
    )
    .await
    else {
        panic!("create result");
    };
    assert_eq!(created.session.title, "Host session");
    assert!(matches!(
        OwnedEndpoint::bind(path.clone()),
        Err(EndpointError::AlreadyRunning { .. })
    ));
    assert!(path.exists());

    drop(client);

    let mut client = connect(&path).await;
    let HostCommandResult::Runtime(RuntimeCommandResult::ShutdownRuntime(stopped)) = request(
        &mut client,
        "stop",
        HostCommand::Runtime(RuntimeCommand::ShutdownRuntime(
            ShutdownRuntimeRequest::default(),
        )),
    )
    .await
    else {
        panic!("shutdown result");
    };
    assert_eq!(
        stopped.lifecycle,
        assistant_protocol::RuntimeLifecycle::Stopped
    );
    server.await.expect("server task").expect("server result");
    assert!(!path.exists());
}

#[tokio::test]
async fn configuration_queries_and_invalid_reload_round_trip_without_secrets() {
    let scripted_model = Arc::new(ScriptedModelService::completing(
        capabilities(false),
        8_192,
        text_message("validation-response", "OK"),
    ));
    let (runtime, source) =
        runtime_with_source(scripted_model.clone(), ToolSetSnapshot::default()).await;
    let mut runtime_events = runtime.subscribe_events();
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("runtime").join("runtime.sock");
    let (shutdown, server) = start_server(path.clone(), runtime).await;
    let mut client = connect(&path).await;

    let HostCommandResult::Runtime(RuntimeCommandResult::GetConfigStatus(status)) = request(
        &mut client,
        "config-status",
        HostCommand::Runtime(RuntimeCommand::GetConfigStatus(
            assistant_protocol::GetConfigStatusRequest::default(),
        )),
    )
    .await
    else {
        panic!("config status result");
    };
    assert_eq!(
        status.status.state,
        assistant_protocol::ConfigurationState::Ready
    );
    assert_eq!(
        status.status.config_path.as_deref(),
        Some("/private/runtime/config.toml")
    );

    let HostCommandResult::Runtime(RuntimeCommandResult::ListModels(models)) = request(
        &mut client,
        "list-models",
        HostCommand::Runtime(RuntimeCommand::ListModels(
            assistant_protocol::ListModelsRequest::default(),
        )),
    )
    .await
    else {
        panic!("list models result");
    };
    assert_eq!(models.models.len(), 1);
    let HostCommandResult::Runtime(RuntimeCommandResult::GetModel(model)) = request(
        &mut client,
        "get-model",
        HostCommand::Runtime(RuntimeCommand::GetModel(
            assistant_protocol::GetModelRequest {
                model_key: assistant_protocol::ModelKey::new("fixture").expect("model key"),
            },
        )),
    )
    .await
    else {
        panic!("get model result");
    };
    assert!(model.model.is_valid);
    assert!(
        !serde_json::to_string(&(&status, &models, &model))
            .expect("serialize projections")
            .contains("unique-test-secret-9f1ca2")
    );

    let HostCommandResult::Runtime(RuntimeCommandResult::ValidateModelConnection(validation)) =
        request(
            &mut client,
            "validate-model",
            HostCommand::Runtime(RuntimeCommand::ValidateModelConnection(
                assistant_protocol::ValidateModelConnectionRequest {
                    model_key: assistant_protocol::ModelKey::new("fixture").expect("model key"),
                },
            )),
        )
        .await
    else {
        panic!("validate model result");
    };
    assert_eq!(
        validation.outcome,
        assistant_protocol::ConnectionValidationOutcome::Succeeded
    );
    let captured = scripted_model.take_requests();
    assert_eq!(captured.len(), 1);
    assert!(captured[0].system.is_empty());
    assert!(captured[0].tools.is_empty());
    assert!(
        !serde_json::to_string(&validation)
            .expect("serialize validation")
            .contains("unique-test-secret-9f1ca2")
    );

    let HostCommandResult::Runtime(RuntimeCommandResult::CreateSession(created)) = request(
        &mut client,
        "create-session",
        HostCommand::Runtime(RuntimeCommand::CreateSession(
            CreateSessionRequest::default(),
        )),
    )
    .await
    else {
        panic!("create session result");
    };
    let event = tokio::time::timeout(Duration::from_secs(1), runtime_events.recv())
        .await
        .expect("session event timeout")
        .expect("session event");
    let host_observation = format!(
        "{} {:?}",
        serde_json::to_string(&(created, event)).expect("serialize host observation"),
        (status, models, model, validation)
    );
    assert!(!host_observation.contains("unique-test-secret-9f1ca2"));

    source.replace(
        "schema_version = 1\ndefault_model = \"fixture\"\napi_key = \"unique-test-secret-9f1ca2\"\n[",
    );
    let HostCommandResult::Runtime(RuntimeCommandResult::ReloadConfig(reloaded)) = request(
        &mut client,
        "reload",
        HostCommand::Runtime(RuntimeCommand::ReloadConfig(
            assistant_protocol::ReloadConfigRequest::default(),
        )),
    )
    .await
    else {
        panic!("reload result");
    };
    assert_eq!(
        reloaded.status.state,
        assistant_protocol::ConfigurationState::Invalid
    );
    let serialized = serde_json::to_string(&reloaded).expect("serialize reload");
    assert!(!serialized.contains("unique-test-secret-9f1ca2"));

    let error = request_error(
        &mut client,
        "create-invalid",
        HostCommand::Runtime(RuntimeCommand::CreateSession(
            CreateSessionRequest::default(),
        )),
    )
    .await;
    assert_eq!(
        error.code,
        assistant_protocol::RuntimeErrorCode::ConfigurationUnavailable
    );
    assert!(
        !format!(
            "{error:?} {error_json}",
            error_json = serde_json::to_string(&error).expect("serialize error")
        )
        .contains("unique-test-secret-9f1ca2")
    );

    drop(client);
    shutdown.cancel();
    server.await.expect("server task").expect("server result");
}

#[tokio::test]
async fn invalid_oversized_frame_closes_only_that_connection() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("runtime").join("runtime.sock");
    let runtime = empty_runtime().await;
    let (shutdown, server) = start_server(path.clone(), runtime).await;
    let mut invalid = connect(&path).await;
    invalid
        .write_all(&((MAX_FRAME_BYTES + 1) as u32).to_be_bytes())
        .await
        .expect("oversized header");
    drop(invalid);

    let client = tokio::time::timeout(Duration::from_secs(1), connect(&path))
        .await
        .expect("server accepts next client");
    drop(client);
    shutdown.cancel();
    server.await.expect("server task").expect("server result");
}

#[tokio::test]
async fn shutdown_request_stops_host_even_if_client_drops_before_reading_response() {
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("runtime").join("runtime.sock");
    let runtime = empty_runtime().await;
    let (_shutdown, server) = start_server(path.clone(), runtime).await;
    let mut client = connect(&path).await;
    write_frame(
        &mut client,
        &ClientFrame::Request {
            request_id: "stop-and-drop".to_owned(),
            command: HostCommand::Runtime(RuntimeCommand::ShutdownRuntime(
                ShutdownRuntimeRequest::default(),
            )),
        },
    )
    .await
    .expect("shutdown request");
    drop(client);

    tokio::time::timeout(Duration::from_secs(1), server)
        .await
        .expect("Host stops")
        .expect("server task")
        .expect("server result");
    assert!(!path.exists());
}

#[tokio::test]
async fn client_disconnect_does_not_cancel_run_and_reconnect_can_query_and_cancel() {
    let entered = Arc::new(Notify::new());
    let cleanup = Arc::new(Notify::new());
    let tool = ScriptedTool::hanging("slow_tool", OrderLog::new())
        .with_entered_signal(entered.clone())
        .with_cleanup_signal(cleanup);
    let mut registry = ToolRegistry::new();
    registry.register(tool).expect("register tool");
    let model = Arc::new(ScriptedModelService::new(
        capabilities(true),
        8_192,
        [ModelScript::Events(message_events(&tool_message()))],
    ));
    let runtime = runtime(model, registry.snapshot()).await;
    let directory = tempdir().expect("tempdir");
    let path = directory.path().join("runtime").join("runtime.sock");
    let (_shutdown, server) = start_server(path.clone(), runtime).await;
    let mut first = connect(&path).await;
    let HostCommandResult::Runtime(RuntimeCommandResult::CreateSession(created)) = request(
        &mut first,
        "create",
        HostCommand::Runtime(RuntimeCommand::CreateSession(
            CreateSessionRequest::default(),
        )),
    )
    .await
    else {
        panic!("create result");
    };
    let session_id = created.session.session_id;
    let HostCommandResult::Runtime(RuntimeCommandResult::StartRun(started)) = request(
        &mut first,
        "start",
        HostCommand::Runtime(RuntimeCommand::StartRun(StartRunRequest {
            session_id: session_id.clone(),
            message: "use the slow tool".to_owned(),
        })),
    )
    .await
    else {
        panic!("start result");
    };
    let run_id = started.run.run_id;
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("tool entered");
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(
                read_frame::<_, ServerFrame>(&mut first)
                    .await
                    .expect("event frame")
                    .expect("connection remains open"),
                ServerFrame::Event {
                    event: assistant_protocol::RuntimeEvent::ToolStarted {
                        run_id: event_run_id,
                        ..
                    }
                } if event_run_id == run_id
            ) {
                break;
            }
        }
    })
    .await
    .expect("real-time tool event reaches client");
    drop(first);

    let mut second = connect(&path).await;
    let HostCommandResult::Runtime(RuntimeCommandResult::GetRun(running)) = request(
        &mut second,
        "get-running",
        HostCommand::Runtime(RuntimeCommand::GetRun(GetRunRequest {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        })),
    )
    .await
    else {
        panic!("get result");
    };
    assert_eq!(running.run.status, RunStatus::Running);
    let HostCommandResult::Runtime(RuntimeCommandResult::CancelRun(cancelling)) = request(
        &mut second,
        "cancel",
        HostCommand::Runtime(RuntimeCommand::CancelRun(CancelRunRequest {
            session_id: session_id.clone(),
            run_id: run_id.clone(),
        })),
    )
    .await
    else {
        panic!("cancel result");
    };
    assert_eq!(cancelling.run.status, RunStatus::Cancelling);

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let HostCommandResult::Runtime(RuntimeCommandResult::GetRun(result)) = request(
                &mut second,
                "get-terminal",
                HostCommand::Runtime(RuntimeCommand::GetRun(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })),
            )
            .await
            else {
                panic!("get terminal result");
            };
            if result.run.status.is_terminal() {
                assert_eq!(result.run.status, RunStatus::Cancelled);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run settles");
    let _ = request(
        &mut second,
        "stop",
        HostCommand::Runtime(RuntimeCommand::ShutdownRuntime(
            ShutdownRuntimeRequest::default(),
        )),
    )
    .await;
    server.await.expect("server task").expect("server result");
}
