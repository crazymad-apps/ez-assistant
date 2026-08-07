//! Feature-gated 私有 Ratatui 客户端；只用于验证正式 Runtime Host。

mod app;
mod ui;

use std::{
    future::pending,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use assistant_protocol::PROTOCOL_VERSION;
use crossterm::event::EventStream;
use futures_util::StreamExt;
use thiserror::Error;
use tokio::{net::UnixStream, sync::mpsc, task::JoinHandle, time::timeout};

use self::app::{DemoApp, DemoEffect};
use crate::{
    config::DemoConfig,
    wire::{ClientFrame, HostCommand, ServerFrame, read_frame, write_frame},
};

const COMMAND_CAPACITY: usize = 32;
const UPDATE_CAPACITY: usize = 256;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn run(config: DemoConfig) -> Result<(), DemoError> {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = ratatui::try_restore();
            return Err(DemoError::Terminal(error));
        }
    };
    let restore = TerminalRestore;
    let result = run_tui(&mut terminal, config.socket_path).await;
    drop(restore);
    result
}

async fn run_tui(
    terminal: &mut ratatui::DefaultTerminal,
    socket_path: PathBuf,
) -> Result<(), DemoError> {
    let mut events = EventStream::new();
    let mut app = DemoApp::default();
    let mut connection = Some(ConnectionRuntime::start(socket_path.clone()));

    loop {
        terminal.draw(|frame| ui::render(frame, &app))?;
        tokio::select! {
            terminal_event = events.next() => {
                let Some(event) = terminal_event else {
                    return Err(DemoError::EventStreamClosed);
                };
                let effects = app.handle_terminal_event(&event?);
                if apply_effects(
                    effects,
                    &mut app,
                    &mut connection,
                    &socket_path,
                ).await {
                    break;
                }
            }
            update = next_connection_update(&mut connection) => {
                let effects = match update {
                    Some(ConnectionUpdate::Connected(runtime_version)) => {
                        app.connected(runtime_version)
                    }
                    Some(ConnectionUpdate::Frame(frame)) => app.handle_server_frame(*frame),
                    Some(ConnectionUpdate::Disconnected(reason)) => {
                        app.disconnected(reason);
                        connection = None;
                        Vec::new()
                    }
                    None => {
                        app.disconnected("connection task stopped");
                        connection = None;
                        Vec::new()
                    }
                };
                if apply_effects(
                    effects,
                    &mut app,
                    &mut connection,
                    &socket_path,
                ).await {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn apply_effects(
    effects: Vec<DemoEffect>,
    app: &mut DemoApp,
    connection: &mut Option<ConnectionRuntime>,
    socket_path: &Path,
) -> bool {
    for effect in effects {
        match effect {
            DemoEffect::Send(command) => {
                let Some(active) = connection.as_ref() else {
                    app.disconnected("no active Runtime connection");
                    continue;
                };
                if active.commands.send(command).await.is_err() {
                    app.disconnected("connection command channel closed");
                    *connection = None;
                }
            }
            DemoEffect::Reconnect => {
                *connection = Some(ConnectionRuntime::start(socket_path.to_path_buf()));
                app.connecting();
            }
            DemoEffect::Quit => return true,
        }
    }
    false
}

async fn next_connection_update(
    connection: &mut Option<ConnectionRuntime>,
) -> Option<ConnectionUpdate> {
    match connection {
        Some(connection) => connection.updates.recv().await,
        None => pending().await,
    }
}

struct ConnectionRuntime {
    commands: mpsc::Sender<HostCommand>,
    updates: mpsc::Receiver<ConnectionUpdate>,
    task: JoinHandle<()>,
}

impl ConnectionRuntime {
    fn start(socket_path: PathBuf) -> Self {
        let (commands, command_receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (update_sender, updates) = mpsc::channel(UPDATE_CAPACITY);
        let task = tokio::spawn(connection_task(
            socket_path,
            command_receiver,
            update_sender,
        ));
        Self {
            commands,
            updates,
            task,
        }
    }
}

impl Drop for ConnectionRuntime {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Debug)]
enum ConnectionUpdate {
    Connected(String),
    Frame(Box<ServerFrame>),
    Disconnected(String),
}

async fn connection_task(
    socket_path: PathBuf,
    mut commands: mpsc::Receiver<HostCommand>,
    updates: mpsc::Sender<ConnectionUpdate>,
) {
    let result = connection_session(socket_path, &mut commands, &updates).await;
    if let Err(reason) = result {
        let _ = updates.send(ConnectionUpdate::Disconnected(reason)).await;
    }
}

async fn connection_session(
    socket_path: PathBuf,
    commands: &mut mpsc::Receiver<HostCommand>,
    updates: &mpsc::Sender<ConnectionUpdate>,
) -> Result<(), String> {
    let mut stream = UnixStream::connect(&socket_path)
        .await
        .map_err(|error| format!("could not connect to {}: {error}", socket_path.display()))?;
    write_frame(
        &mut stream,
        &ClientFrame::Hello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "runtime-demo-tui".to_owned(),
        },
    )
    .await
    .map_err(|error| format!("handshake write failed: {error}"))?;
    let frame = timeout(HANDSHAKE_TIMEOUT, read_frame::<_, ServerFrame>(&mut stream))
        .await
        .map_err(|_| "Runtime handshake timed out".to_owned())?
        .map_err(|error| format!("handshake read failed: {error}"))?;
    let runtime_version = match frame {
        Some(ServerFrame::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            runtime_version,
        }) => runtime_version,
        Some(ServerFrame::Error { error, .. }) => {
            return Err(format!("Runtime rejected handshake: {}", error.message));
        }
        Some(_) => return Err("Runtime returned an invalid handshake response".to_owned()),
        None => return Err("Runtime closed during handshake".to_owned()),
    };
    updates
        .send(ConnectionUpdate::Connected(runtime_version))
        .await
        .map_err(|_| "TUI update channel closed".to_owned())?;

    let (mut reader, mut writer) = stream.into_split();
    let mut request_sequence = 0_u64;
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                request_sequence = request_sequence.saturating_add(1);
                write_frame(
                    &mut writer,
                    &ClientFrame::Request {
                        request_id: format!("demo-{request_sequence}"),
                        command,
                    },
                )
                .await
                .map_err(|error| format!("request write failed: {error}"))?;
            }
            frame = read_frame::<_, ServerFrame>(&mut reader) => {
                let Some(frame) = frame
                    .map_err(|error| format!("Runtime read failed: {error}"))?
                else {
                    return Err("Runtime connection closed".to_owned());
                };
                if matches!(
                    frame,
                    ServerFrame::Event {
                        event: assistant_protocol::RuntimeEvent::TextDelta { .. }
                            | assistant_protocol::RuntimeEvent::ReasoningDelta { .. }
                            | assistant_protocol::RuntimeEvent::ToolOutput { .. },
                    }
                ) {
                    // 高频增量允许因在线背压丢失；终态等控制事件仍可靠送入展示层，
                    // 随后的快照查询会校准增量投影。
                    let _ = updates.try_send(ConnectionUpdate::Frame(Box::new(frame)));
                } else {
                    updates
                        .send(ConnectionUpdate::Frame(Box::new(frame)))
                        .await
                        .map_err(|_| "TUI update channel closed".to_owned())?;
                }
            }
        }
    }
}

struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = ratatui::try_restore();
    }
}

#[derive(Debug, Error)]
pub(crate) enum DemoError {
    #[error("terminal operation failed: {0}")]
    Terminal(#[from] io::Error),
    #[error("terminal event stream closed")]
    EventStreamClosed,
}
