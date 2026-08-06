//! 单活动客户端 Unix Socket server 与 Runtime 命令/事件桥接。

use std::{sync::Arc, time::Duration};

use assistant_protocol::{
    PROTOCOL_VERSION, RuntimeCommand, RuntimeCommandResult, RuntimeErrorCode, RuntimeErrorInfo,
    ShutdownRuntimeRequest,
};
use assistant_runtime::AssistantRuntime;
use thiserror::Error;
use tokio::{
    net::UnixStream,
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tokio_util::sync::CancellationToken;

use crate::{
    endpoint::OwnedEndpoint,
    wire::{ClientFrame, HostCommand, HostCommandResult, ServerFrame, read_frame, write_frame},
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_QUEUE_CAPACITY: usize = 16;
const EVENT_QUEUE_CAPACITY: usize = 64;

pub(crate) struct RuntimeServer {
    endpoint: OwnedEndpoint,
    runtime: Arc<AssistantRuntime>,
}

impl RuntimeServer {
    pub(crate) fn new(endpoint: OwnedEndpoint, runtime: Arc<AssistantRuntime>) -> Self {
        Self { endpoint, runtime }
    }

    pub(crate) async fn serve(self) -> Result<(), ServerError> {
        let shutdown = CancellationToken::new();
        let signal = shutdown.clone();
        let signal_task = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal.cancel();
            }
        });
        let result = self.serve_until(shutdown).await;
        signal_task.abort();
        let _ = signal_task.await;
        result
    }

    pub(crate) async fn serve_until(self, shutdown: CancellationToken) -> Result<(), ServerError> {
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = self.endpoint.listener().accept() => accepted,
            };
            let (stream, _) = accepted.map_err(ServerError::Accept)?;
            match serve_connection(self.runtime.clone(), stream, shutdown.clone()).await {
                ConnectionEnd::Disconnected => {}
                ConnectionEnd::ShutdownRequested => {
                    shutdown.cancel();
                    break;
                }
            }
        }
        self.runtime
            .shutdown(ShutdownRuntimeRequest::default())
            .await
            .map_err(|error| ServerError::Runtime(error.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub(crate) enum ServerError {
    #[error("runtime endpoint accept failed: {0}")]
    Accept(std::io::Error),
    #[error("runtime controlled shutdown failed: {0}")]
    Runtime(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConnectionEnd {
    Disconnected,
    ShutdownRequested,
}

struct ReliableFrame {
    frame: ServerFrame,
    flushed: oneshot::Sender<Result<(), String>>,
}

async fn serve_connection(
    runtime: Arc<AssistantRuntime>,
    mut stream: UnixStream,
    host_shutdown: CancellationToken,
) -> ConnectionEnd {
    if !handshake(&mut stream).await {
        return ConnectionEnd::Disconnected;
    }
    let (mut reader, writer) = stream.into_split();
    let connection = host_shutdown.child_token();
    let (response_tx, response_rx) = mpsc::channel(RESPONSE_QUEUE_CAPACITY);
    let (event_tx, event_rx) = mpsc::channel(EVENT_QUEUE_CAPACITY);
    let writer_task = spawn_writer(writer, response_rx, event_rx, connection.clone());
    let event_task = spawn_event_forwarder(runtime.clone(), event_tx.clone(), connection.clone());
    let mut end = ConnectionEnd::Disconnected;

    loop {
        let incoming = tokio::select! {
            () = connection.cancelled() => break,
            incoming = read_frame::<_, ClientFrame>(&mut reader) => incoming,
        };
        let frame = match incoming {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let ClientFrame::Request {
            request_id,
            command,
        } = frame
        else {
            break;
        };
        if request_id.trim().is_empty() {
            break;
        }
        let (frame, shutdown_requested) = dispatch(&runtime, request_id, command).await;
        if shutdown_requested {
            end = ConnectionEnd::ShutdownRequested;
        }
        if !send_reliable(&response_tx, frame, &connection).await {
            break;
        }
        if shutdown_requested {
            break;
        }
    }

    connection.cancel();
    drop(response_tx);
    drop(event_tx);
    let _ = event_task.await;
    let _ = writer_task.await;
    end
}

async fn handshake(stream: &mut UnixStream) -> bool {
    let incoming = timeout(HANDSHAKE_TIMEOUT, read_frame::<_, ClientFrame>(stream)).await;
    let Ok(Ok(Some(ClientFrame::Hello {
        protocol_version,
        client_name,
    }))) = incoming
    else {
        return false;
    };
    if protocol_version != PROTOCOL_VERSION || client_name.trim().is_empty() {
        let _ = write_frame(
            stream,
            &ServerFrame::Error {
                request_id: "handshake".to_owned(),
                error: RuntimeErrorInfo::new(
                    RuntimeErrorCode::InvalidRequest,
                    "runtime protocol version or client name is invalid",
                ),
            },
        )
        .await;
        return false;
    }
    write_frame(
        stream,
        &ServerFrame::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
    )
    .await
    .is_ok()
}

async fn dispatch(
    runtime: &AssistantRuntime,
    request_id: String,
    command: HostCommand,
) -> (ServerFrame, bool) {
    let result = match command {
        HostCommand::Runtime(command) => dispatch_runtime(runtime, command).await,
        HostCommand::ConversationSnapshot { session_id } => runtime
            .conversation_snapshot(&session_id)
            .map(|conversation| {
                (
                    HostCommandResult::ConversationSnapshot { conversation },
                    false,
                )
            }),
    };
    match result {
        Ok((result, shutdown)) => (ServerFrame::Response { request_id, result }, shutdown),
        Err(error) => (
            ServerFrame::Error {
                request_id,
                error: error.to_protocol_info(),
            },
            false,
        ),
    }
}

async fn dispatch_runtime(
    runtime: &AssistantRuntime,
    command: RuntimeCommand,
) -> Result<(HostCommandResult, bool), assistant_runtime::RuntimeError> {
    let (result, shutdown) = match command {
        RuntimeCommand::CreateSession(request) => (
            RuntimeCommandResult::CreateSession(runtime.create_session(request)?),
            false,
        ),
        RuntimeCommand::ListSessions(request) => (
            RuntimeCommandResult::ListSessions(runtime.list_sessions(request)?),
            false,
        ),
        RuntimeCommand::GetSession(request) => (
            RuntimeCommandResult::GetSession(runtime.get_session(request)?),
            false,
        ),
        RuntimeCommand::StartRun(request) => (
            RuntimeCommandResult::StartRun(runtime.start_run(request)?),
            false,
        ),
        RuntimeCommand::GetRun(request) => (
            RuntimeCommandResult::GetRun(runtime.get_run(request)?),
            false,
        ),
        RuntimeCommand::CancelRun(request) => (
            RuntimeCommandResult::CancelRun(runtime.cancel_run(request)?),
            false,
        ),
        RuntimeCommand::ShutdownRuntime(request) => (
            RuntimeCommandResult::ShutdownRuntime(runtime.shutdown(request).await?),
            true,
        ),
    };
    Ok((HostCommandResult::Runtime(result), shutdown))
}

fn spawn_writer(
    mut writer: tokio::net::unix::OwnedWriteHalf,
    mut responses: mpsc::Receiver<ReliableFrame>,
    mut events: mpsc::Receiver<ServerFrame>,
    cancellation: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                Some(response) = responses.recv() => {
                    let result = tokio::select! {
                        () = cancellation.cancelled() => Err("connection closed".to_owned()),
                        result = write_frame(&mut writer, &response.frame) => {
                            result.map_err(|error| error.to_string())
                        }
                    };
                    let failed = result.is_err();
                    let _ = response.flushed.send(result);
                    if failed {
                        cancellation.cancel();
                        break;
                    }
                }
                Some(event) = events.recv() => {
                    let result = tokio::select! {
                        () = cancellation.cancelled() => break,
                        result = write_frame(&mut writer, &event) => result,
                    };
                    if result.is_err() {
                        cancellation.cancel();
                        break;
                    }
                }
                else => break,
            }
        }
    })
}

fn spawn_event_forwarder(
    runtime: Arc<AssistantRuntime>,
    events: mpsc::Sender<ServerFrame>,
    cancellation: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut receiver = runtime.subscribe_events();
        loop {
            let event = tokio::select! {
                () = cancellation.cancelled() => break,
                event = receiver.recv() => event,
            };
            match event {
                Ok(event) => match events.try_send(ServerFrame::Event { event }) {
                    Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => {}
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

async fn send_reliable(
    sender: &mpsc::Sender<ReliableFrame>,
    frame: ServerFrame,
    cancellation: &CancellationToken,
) -> bool {
    let (flushed, receiver) = oneshot::channel();
    let queued = ReliableFrame { frame, flushed };
    let sent = tokio::select! {
        () = cancellation.cancelled() => return false,
        sent = sender.send(queued) => sent,
    };
    if sent.is_err() {
        return false;
    }
    matches!(
        tokio::select! {
            () = cancellation.cancelled() => return false,
            result = receiver => result,
        },
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, sync::Arc, time::Duration};

    use agent_model::{ModelCapabilities, ModelService};
    use agent_sdk::{
        Agent, AgentBuilder, AllowAllAuthorizer, ContextWindowEvaluator, SystemPromptSnapshot,
    };
    use agent_testkit::{
        ModelScript, OrderLog, ScriptedModelService, ScriptedTool, message_events,
    };
    use agent_tools::{ToolRegistry, ToolSetSnapshot};
    use agent_types::{
        AssistantMessage, AssistantPart, FinishReason, MessageId, ModelIdentity, ProviderId,
        ToolCall, ToolCallId, ToolName,
    };
    use assistant_protocol::{
        CancelRunRequest, CreateSessionRequest, GetRunRequest, RunStatus, StartRunRequest,
    };
    use assistant_runtime::{AgentFactoryError, RuntimeConfig, SessionAgentFactory};
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::{io::AsyncWriteExt, sync::Notify};

    use super::*;
    use crate::{
        endpoint::{EndpointError, OwnedEndpoint},
        wire::{HostCommand, HostCommandResult, MAX_FRAME_BYTES},
    };

    struct StaticFactory {
        model: Arc<dyn ModelService>,
        tools: ToolSetSnapshot,
    }

    impl SessionAgentFactory for StaticFactory {
        fn create_agent(&self) -> Result<Agent, AgentFactoryError> {
            AgentBuilder::new(
                self.model.clone(),
                SystemPromptSnapshot::new(vec!["Host test agent".to_owned()]),
                Arc::new(ContextWindowEvaluator::new(0.8).expect("threshold")),
            )
            .tools(self.tools.clone())
            .build()
            .map_err(|source| AgentFactoryError::with_source("test agent build failed", source))
        }
    }

    fn capabilities(has_tools: bool) -> ModelCapabilities {
        ModelCapabilities {
            reasoning: false,
            tool_calls: has_tools,
            streaming: true,
        }
    }

    fn runtime(model: Arc<dyn ModelService>, tools: ToolSetSnapshot) -> Arc<AssistantRuntime> {
        Arc::new(AssistantRuntime::new(
            RuntimeConfig::new(NonZeroUsize::new(64).expect("capacity")),
            Arc::new(StaticFactory { model, tools }),
            Arc::new(AllowAllAuthorizer),
        ))
    }

    fn empty_runtime() -> Arc<AssistantRuntime> {
        runtime(
            Arc::new(ScriptedModelService::new(capabilities(false), 8_192, [])),
            ToolSetSnapshot::default(),
        )
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
        let runtime = empty_runtime();
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
    async fn invalid_oversized_frame_closes_only_that_connection() {
        let directory = tempdir().expect("tempdir");
        let path = directory.path().join("runtime").join("runtime.sock");
        let runtime = empty_runtime();
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
        let runtime = empty_runtime();
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
        let runtime = runtime(model, registry.snapshot());
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
}
