use std::{
    num::NonZeroUsize,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent_model::{ModelCapabilities, ModelService, SystemPromptSnapshot};
use agent_sdk::AllowAllAuthorizer;
use agent_testkit::{ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events};
use agent_tools::{ToolRegistry, ToolSetSnapshot};
use agent_types::{
    AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, PartId, ProviderId,
    TextPart, ToolCall, ToolCallId, ToolName, UserMessage, UserPart,
};
use assistant_protocol::{
    CancelRunRequest, CreateSessionRequest, GetRunRequest, GetSessionRequest, IdempotencyKey,
    InputId, ListSessionsRequest, ModelKey, PROTOCOL_VERSION, ResumeSessionRequest,
    RetryRunRequest, RunId, RunStatus, RuntimeCommand, RuntimeCommandResult, RuntimeErrorCode,
    SessionId, ShutdownRuntimeRequest, SubmitInputRequest,
};
use assistant_runtime::{
    AssistantRuntime, ConfigSourceFuture, ConfigSourceLoad, ModelServiceFactory,
    ModelServiceFactoryError, ModelServiceFactoryRequest, NewStoredInput, NewStoredSession,
    RuntimeConfig, RuntimeConfigSource, RuntimeStore, SystemPromptFactory,
    SystemPromptFactoryError, UserMessageCommit,
};
use serde_json::json;
use tempfile::tempdir;
use tokio::{io::AsyncWriteExt, net::UnixStream, sync::Notify, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::{RuntimeServer, ServerError};
use crate::{
    endpoint::{EndpointError, OwnedEndpoint},
    storage::LocalRuntimeStore,
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

async fn persistent_runtime(
    runtime_home: &Path,
    model: Arc<dyn ModelService>,
) -> Arc<AssistantRuntime> {
    let source = Arc::new(MutableConfigSource::new(TEST_CONFIG));
    let store = Arc::new(
        LocalRuntimeStore::open(runtime_home, 8)
            .await
            .expect("open persistent store"),
    );
    let runtime = Arc::new(
        AssistantRuntime::open(
            RuntimeConfig::new(NonZeroUsize::new(64).expect("capacity")),
            source,
            Arc::new(StaticModelFactory { model }),
            Arc::new(StaticSystemPromptFactory),
            ToolSetSnapshot::default(),
            Arc::new(AllowAllAuthorizer),
            store,
        )
        .await
        .expect("recover persistent runtime"),
    );
    runtime
        .reload_config(assistant_protocol::ReloadConfigRequest::default())
        .await
        .expect("load config");
    runtime
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
    let HostCommandResult::Runtime(RuntimeCommandResult::SubmitInput(started)) = request(
        &mut first,
        "start",
        HostCommand::Runtime(RuntimeCommand::SubmitInput(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "use the slow tool".to_owned(),
            idempotency_key: None,
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

#[tokio::test]
async fn runtime_reopens_persisted_session_conversation_and_terminal_run() {
    let root = tempdir().expect("tempdir");
    let final_message = text_message("assistant-persisted", "persisted answer");
    let first_model = Arc::new(ScriptedModelService::completing(
        capabilities(false),
        8_192,
        final_message.clone(),
    ));
    let first = persistent_runtime(root.path(), first_model).await;
    let created = first
        .create_session(CreateSessionRequest {
            title: Some("Persistent Session".to_owned()),
            model_key: None,
        })
        .await
        .expect("create persisted session");
    let started = first
        .submit_input(SubmitInputRequest {
            session_id: created.session.session_id.clone(),
            message: "persist this".to_owned(),
            idempotency_key: None,
        })
        .await
        .expect("start persisted run");
    let terminal = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let run = first
                .get_run(GetRunRequest {
                    session_id: created.session.session_id.clone(),
                    run_id: started.run.run_id.clone(),
                })
                .await
                .expect("query persisted run")
                .run;
            if run.status.is_terminal() {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("run reaches terminal state");
    assert_eq!(terminal.status, RunStatus::Completed);
    assert_eq!(terminal.text, "persisted answer");
    first
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown first runtime");
    drop(first);

    let unused_model = Arc::new(ScriptedModelService::new(capabilities(false), 8_192, []));
    let reopened = persistent_runtime(root.path(), unused_model.clone()).await;
    let listed = reopened
        .list_sessions(ListSessionsRequest::default())
        .expect("list recovered sessions");
    assert_eq!(listed.sessions.len(), 1);
    assert_eq!(listed.sessions[0].session_id, created.session.session_id);
    assert_eq!(listed.sessions[0].message_count, 2);
    assert_eq!(
        reopened
            .get_session(GetSessionRequest {
                session_id: created.session.session_id.clone(),
            })
            .expect("get recovered session")
            .session
            .title,
        "Persistent Session"
    );
    let conversation = reopened
        .conversation_snapshot(&created.session.session_id)
        .await
        .expect("load recovered conversation");
    assert_eq!(conversation.messages.len(), 2);
    assert_eq!(
        conversation.messages[1],
        agent_types::ConversationMessage::Assistant(final_message)
    );
    let recovered_run = reopened
        .get_run(GetRunRequest {
            session_id: created.session.session_id.clone(),
            run_id: started.run.run_id,
        })
        .await
        .expect("get recovered run")
        .run;
    assert_eq!(recovered_run.status, RunStatus::Completed);
    assert_eq!(recovered_run.text, "persisted answer");
    assert!(unused_model.take_requests().is_empty());
    reopened
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown reopened runtime");
}

#[tokio::test]
async fn recovered_queued_input_waits_for_explicit_resume() {
    let root = tempdir().expect("tempdir");
    let store = LocalRuntimeStore::open(root.path(), 4)
        .await
        .expect("open seed store");
    let session_id = SessionId::new("s-resume").expect("session id");
    store
        .create_session(NewStoredSession {
            session_id: session_id.clone(),
            title: "Resume Session".to_owned(),
            model_key: ModelKey::new("fixture").expect("model key"),
            system_prompt: SystemPromptSnapshot::new(vec!["stable prompt".to_owned()]),
            created_at_ms: 1_000,
        })
        .await
        .expect("seed session");
    let input_id = InputId::new("i-resume").expect("input id");
    let run_id = RunId::new("r-resume").expect("run id");
    store
        .accept_input(NewStoredInput {
            input_id: input_id.clone(),
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            idempotency_key: Some(IdempotencyKey::new("resume-key").expect("key")),
            message: UserMessage {
                id: MessageId::new("m-resume").expect("message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("p-resume").expect("part id"),
                    text: "resume me".to_owned(),
                })],
            },
            accepted_at_ms: 2_000,
        })
        .await
        .expect("seed queued input");
    store.shutdown().await.expect("close seed store");

    let model = Arc::new(ScriptedModelService::completing(
        capabilities(false),
        8_192,
        text_message("a-resume", "resumed"),
    ));
    let runtime = persistent_runtime(root.path(), model.clone()).await;
    let summary = runtime
        .get_session(GetSessionRequest {
            session_id: session_id.clone(),
        })
        .expect("recovered session")
        .session;
    assert!(summary.resume_required);
    assert_eq!(summary.queued_input_count, 1);
    assert!(model.take_requests().is_empty());
    assert!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("empty conversation")
            .messages
            .is_empty()
    );
    let repeated = runtime
        .submit_input(SubmitInputRequest {
            session_id: session_id.clone(),
            message: "ignored retry payload".to_owned(),
            idempotency_key: Some(IdempotencyKey::new("resume-key").expect("key")),
        })
        .await
        .expect("idempotent retry");
    assert_eq!(repeated.input_id, input_id);
    assert_eq!(repeated.run.run_id, run_id);
    assert!(model.take_requests().is_empty());

    runtime
        .resume_session(ResumeSessionRequest {
            session_id: session_id.clone(),
        })
        .await
        .expect("resume session");
    let terminal = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let run = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                })
                .await
                .expect("run")
                .run;
            if run.status.is_terminal() {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("resumed run finishes");
    assert_eq!(terminal.status, RunStatus::Completed);
    assert_eq!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("conversation")
            .messages
            .len(),
        2
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown runtime");
}

#[tokio::test]
async fn retrying_a_recovered_interrupted_run_does_not_duplicate_the_user_message() {
    let root = tempdir().expect("tempdir");
    let store = LocalRuntimeStore::open(root.path(), 4)
        .await
        .expect("open seed store");
    let session_id = SessionId::new("s-retry").expect("session id");
    let input_id = InputId::new("i-retry").expect("input id");
    let run_id = RunId::new("r-retry-1").expect("run id");
    let message = UserMessage {
        id: MessageId::new("m-retry").expect("message id"),
        parts: vec![UserPart::Text(TextPart {
            id: PartId::new("p-retry").expect("part id"),
            text: "retry after restart".to_owned(),
        })],
    };
    store
        .create_session(NewStoredSession {
            session_id: session_id.clone(),
            title: "Retry Session".to_owned(),
            model_key: ModelKey::new("fixture").expect("model key"),
            system_prompt: SystemPromptSnapshot::new(vec!["stable prompt".to_owned()]),
            created_at_ms: 1_000,
        })
        .await
        .expect("seed session");
    store
        .accept_input(NewStoredInput {
            input_id: input_id.clone(),
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            idempotency_key: None,
            message: message.clone(),
            accepted_at_ms: 2_000,
        })
        .await
        .expect("accept input");
    store
        .commit_user_message(UserMessageCommit {
            operation_id: "append-retry".to_owned(),
            input_id: input_id.clone(),
            run_id: run_id.clone(),
            session_id: session_id.clone(),
            message: Some(message),
            created_at_ms: 3_000,
        })
        .await
        .expect("start run durably");
    let queued_input_id = InputId::new("i-still-queued").expect("input id");
    let queued_run_id = RunId::new("r-still-queued").expect("run id");
    store
        .accept_input(NewStoredInput {
            input_id: queued_input_id.clone(),
            run_id: queued_run_id.clone(),
            session_id: session_id.clone(),
            idempotency_key: None,
            message: UserMessage {
                id: MessageId::new("m-still-queued").expect("message id"),
                parts: vec![UserPart::Text(TextPart {
                    id: PartId::new("p-still-queued").expect("part id"),
                    text: "do not resume me".to_owned(),
                })],
            },
            accepted_at_ms: 4_000,
        })
        .await
        .expect("accept later queued input");
    store.shutdown().await.expect("close seed store");

    let model = Arc::new(ScriptedModelService::completing(
        capabilities(false),
        8_192,
        text_message("a-retry", "retried"),
    ));
    let runtime = persistent_runtime(root.path(), model.clone()).await;
    assert_eq!(
        runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: run_id.clone()
            })
            .await
            .expect("interrupted run")
            .run
            .status,
        RunStatus::Interrupted
    );
    assert_eq!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("conversation")
            .messages
            .len(),
        1
    );
    let retry = runtime
        .retry_run(RetryRunRequest {
            session_id: session_id.clone(),
            run_id,
        })
        .await
        .expect("retry interrupted run");
    assert_eq!(retry.run.input_id, input_id);
    assert_eq!(retry.run.attempt, 2);
    let terminal = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let run = runtime
                .get_run(GetRunRequest {
                    session_id: session_id.clone(),
                    run_id: retry.run.run_id.clone(),
                })
                .await
                .expect("retry run")
                .run;
            if run.status.is_terminal() {
                break run;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry finishes");
    assert_eq!(terminal.status, RunStatus::Completed);
    assert_eq!(model.take_requests().len(), 1);
    assert_eq!(
        runtime
            .get_run(GetRunRequest {
                session_id: session_id.clone(),
                run_id: queued_run_id,
            })
            .await
            .expect("later queued run")
            .run
            .status,
        RunStatus::Accepted
    );
    let summary = runtime
        .get_session(GetSessionRequest {
            session_id: session_id.clone(),
        })
        .expect("session")
        .session;
    assert!(summary.resume_required);
    assert_eq!(summary.queued_input_count, 1);
    assert_eq!(
        runtime
            .conversation_snapshot(&session_id)
            .await
            .expect("conversation")
            .messages
            .len(),
        2
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown runtime");
}

#[tokio::test]
async fn corrupt_conversation_is_isolated_and_never_replaced_with_empty_state() {
    let root = tempdir().expect("tempdir");
    let store = LocalRuntimeStore::open(root.path(), 4)
        .await
        .expect("open seed store");
    let session_id = SessionId::new("s-unavailable").expect("session id");
    store
        .create_session(NewStoredSession {
            session_id: session_id.clone(),
            title: "Unavailable Session".to_owned(),
            model_key: ModelKey::new("fixture").expect("model key"),
            system_prompt: SystemPromptSnapshot::new(vec!["stable prompt".to_owned()]),
            created_at_ms: 1_000,
        })
        .await
        .expect("seed session");
    store.shutdown().await.expect("close seed store");
    let body = root
        .path()
        .join("data/sessions/s-unavailable/conversation.1.jsonl");
    std::fs::write(&body, b"not-json\n").expect("corrupt conversation fixture");

    let runtime = persistent_runtime(
        root.path(),
        Arc::new(ScriptedModelService::new(capabilities(false), 8_192, [])),
    )
    .await;
    assert_eq!(
        runtime
            .list_sessions(ListSessionsRequest::default())
            .expect("list recovered session")
            .sessions[0]
            .message_count,
        0
    );
    for _ in 0..2 {
        let error = runtime
            .conversation_snapshot(&session_id)
            .await
            .expect_err("corrupt conversation remains unavailable");
        assert_eq!(
            error.to_protocol_info().code,
            RuntimeErrorCode::StorageUnavailable
        );
    }
    assert_eq!(
        std::fs::read(&body).expect("read corrupt fixture"),
        b"not-json\n"
    );
    runtime
        .shutdown(ShutdownRuntimeRequest::default())
        .await
        .expect("shutdown runtime");
}
