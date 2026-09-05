//! Device Gateway 的进程内所有权、启停、发现与管理投影。

mod playback;

use playback::PreparedPlayback;

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    ConfirmDevicePairingRequest, DeviceCapabilitiesSnapshot, DeviceConnectionSnapshot,
    DeviceGatewayEvent, DeviceGatewaySnapshot, DeviceLifecycleSnapshot,
    DevicePairingWindowSnapshot, DeviceSummarySnapshot, OutputPreferenceSnapshot,
    PendingDevicePairingSnapshot, RuntimeErrorCode, RuntimeErrorInfo,
};
use assistant_runtime::{
    AssistantRuntime, ChannelOutput, ChannelOutputDispatchError, ChannelSpeechSegment,
    DeviceDeliveryPreference, DeviceLifecycle, OutputPreference, ResolvedChannelDelivery,
};
use axum::{Router, routing::get};
use axum_server::{Handle as ServerHandle, tls_rustls::RustlsConfig};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use thiserror::Error;
use tokio::{
    sync::{Mutex, broadcast, mpsc, oneshot},
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::identity::{IdentityError, InstallationIdentity};
use super::{
    dispatcher::DeviceChannelOutputDispatcher,
    protocol::{InteractionStateChanged, TextOutput, preference_is_supported},
};
use crate::speech::{SpeechServiceError, SpeechServiceHandle};

const PAIRING_WINDOW_DURATION: Duration = Duration::from_secs(5 * 60);
const MAX_PAIRING_ATTEMPTS: u8 = 5;
const LIFECYCLE_QUEUE_CAPACITY: usize = 16;
const CONNECTION_COMMAND_CAPACITY: usize = 32;
const GATEWAY_EVENT_CAPACITY: usize = 32;
const DEVICE_SERVICE_TYPE: &str = "_ez-assistant._tcp.local.";

/// 供 HTTP 管理入口和 Runtime 渠道分发器调用的 Gateway 轻量句柄。
///
/// 句柄不拥有 listener 生命周期；启停命令由 [`DeviceGatewayService`] 串行处理。
#[derive(Clone)]
pub(crate) struct DeviceGatewayHandle {
    shared: Arc<GatewayShared>,
    lifecycle: mpsc::Sender<LifecycleCommand>,
}

/// Device Gateway 长期子系统，拥有 WSS listener、mDNS 发布及其启停生命周期。
///
/// 稳定设备登记仍由 Runtime/Store 持有，Service 只组合 Host 易失状态。
pub(crate) struct DeviceGatewayService {
    runtime_home: PathBuf,
    shared: Arc<GatewayShared>,
    lifecycle: mpsc::Receiver<LifecycleCommand>,
    speech_status: tokio::sync::watch::Receiver<assistant_protocol::DeviceSpeechServicesSnapshot>,
}

/// 所有设备连接共享的 Host 资源。
///
/// `state` 是在线连接和配对候选的唯一 owner；`voice_turns` 按设备而非 WSS 连接保存，
/// 使一次语音轮次可以跨连接续传而不复制到 Runtime。`recognition_tasks` 属于 Gateway
/// 生命周期，确保跨连接 ASR 子任务在关闭时可取消、等待且不会静默丢失 panic。
pub(super) struct GatewayShared {
    pub(super) runtime: Arc<AssistantRuntime>,
    pub(super) speech: SpeechServiceHandle,
    state: Mutex<GatewayState>,
    voice_turns: Mutex<
        HashMap<
            assistant_protocol::DeviceId,
            Arc<Mutex<Option<super::connection::VoiceTurnAggregation>>>,
        >,
    >,
    pub(super) recognition_tasks: TaskTracker,
    events: broadcast::Sender<DeviceGatewayEvent>,
}

/// Gateway 当前进程内状态；关闭 Host 后全部失效。
struct GatewayState {
    enabled: bool,
    available: bool,
    identity: Option<Arc<InstallationIdentity>>,
    pairing_window_expires_at_ms: Option<i64>,
    pending_pairings: HashMap<String, PendingPairing>,
    connections: HashMap<assistant_protocol::DeviceId, ActiveConnection>,
}

/// 等待 Desktop 输入配对码的候选设备及其一次性决策通道。
pub(super) struct PendingPairing {
    pub(super) display_name: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    pub(super) expires_at_ms: i64,
    pub(super) remaining_attempts: u8,
    pub(super) decision: mpsc::Sender<PairingDecision>,
}

/// Desktop 对候选设备作出的配对确认，不代表设备身份已经持久化。
pub(super) struct PairingDecision {
    pub(super) pairing_code: String,
    pub(super) display_name: Option<String>,
}

/// 一台已认证设备的当前唯一在线连接。
///
/// 新连接替换旧连接时通过 `command` 通知旧连接收敛，避免同一设备并行接收输出。
struct ActiveConnection {
    connection_id: String,
    connected_at_ms: i64,
    capabilities: DeviceCapabilitiesSnapshot,
    output_preference: OutputPreferenceSnapshot,
    command: mpsc::Sender<ConnectionCommand>,
}

/// Gateway 向单个连接串行发送的生命周期和输出控制命令。
pub(super) enum ConnectionCommand {
    Replaced,
    Revoked,
    GatewayDisabled,
    TextOutput(TextOutput),
    OutputUnavailable(InteractionStateChanged),
    PreparePlayback(PlaybackPreparation),
    StartPlayback {
        output: PlaybackOutput,
        response: oneshot::Sender<bool>,
    },
}

/// 为一个播报片段预留设备播放队列容量的请求。
///
/// 预留先于 TTS 执行，避免生成完成后才发现设备队列已满。
pub(super) struct PlaybackPreparation {
    pub(super) output_id: String,
    pub(super) cancellation: CancellationToken,
    pub(super) response: oneshot::Sender<PlaybackPreparationResult>,
}

/// 设备播放队列预留结果。
pub(super) enum PlaybackPreparationResult {
    /// 已占用一个队列槽位，后续可提交相同 `output_id` 的 PCM。
    Accepted,
    /// 所属输出周期已取消，调用方不应继续合成。
    Interrupted,
    /// 当前连接的有界播放队列已满。
    CapacityExceeded,
}

/// TTS 完成后附加到已预留播放槽位的输出数据。
pub(super) struct PlaybackOutput {
    pub(super) output_id: String,
    pub(super) run_id: assistant_protocol::RunId,
    pub(super) text: String,
    pub(super) pcm: Arc<[u8]>,
}

/// 串行驱动 Gateway 启停和发现刷新的内部命令。
enum LifecycleCommand {
    SetEnabled {
        enabled: bool,
        response: oneshot::Sender<Result<(), DeviceGatewayError>>,
    },
    RefreshDiscovery,
}

/// 一次已启动的 listener 与 mDNS 发布资源；停用时必须成组回收。
struct ActiveGateway {
    server_handle: ServerHandle<SocketAddr>,
    server_task: JoinHandle<Result<(), std::io::Error>>,
    mdns: ServiceDaemon,
    service_fullname: String,
    port: u16,
}

impl DeviceGatewayService {
    pub(crate) fn new(
        runtime_home: PathBuf,
        runtime: Arc<AssistantRuntime>,
        dispatcher: &DeviceChannelOutputDispatcher,
        speech: SpeechServiceHandle,
    ) -> (Self, DeviceGatewayHandle) {
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel(LIFECYCLE_QUEUE_CAPACITY);
        let (event_tx, _) = broadcast::channel(GATEWAY_EVENT_CAPACITY);
        let shared = Arc::new(GatewayShared {
            runtime,
            speech,
            events: event_tx,
            voice_turns: Mutex::new(HashMap::new()),
            recognition_tasks: TaskTracker::new(),
            state: Mutex::new(GatewayState {
                enabled: false,
                available: false,
                identity: None,
                pairing_window_expires_at_ms: None,
                pending_pairings: HashMap::new(),
                connections: HashMap::new(),
            }),
        });
        let speech_status = shared.speech.subscribe_status();
        dispatcher.bind(&shared);
        (
            Self {
                runtime_home,
                shared: shared.clone(),
                lifecycle: lifecycle_rx,
                speech_status,
            },
            DeviceGatewayHandle {
                shared,
                lifecycle: lifecycle_tx,
            },
        )
    }

    pub(crate) async fn run_until(
        mut self,
        shutdown: CancellationToken,
    ) -> Result<(), DeviceGatewayError> {
        let mut active = None;
        let mut speech_status_open = true;
        let mut pairing_expiry = interval(Duration::from_secs(1));
        pairing_expiry.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                command = self.lifecycle.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    match command {
                        LifecycleCommand::SetEnabled { enabled, response } => {
                            let result = if enabled {
                                if active.is_none() {
                                    self.start_gateway().await.map(|started| active = Some(started))
                                } else {
                                    Ok(())
                                }
                            } else {
                                if let Some(started) = active.take() {
                                    self.stop_gateway(started).await;
                                }
                                self.mark_disabled().await;
                                Ok(())
                            };
                            let _ = response.send(result);
                        }
                        LifecycleCommand::RefreshDiscovery => {
                            if let Some(started) = active.as_mut()
                                && let Err(error) = self.refresh_discovery(started).await
                            {
                                self.shared.state.lock().await.available = false;
                                self.shared.notify_changed();
                                eprintln!("runtime-host: device discovery degraded: {error}");
                            }
                        }
                    }
                }
                _ = pairing_expiry.tick() => {
                    let expired = {
                        let mut state = self.shared.state.lock().await;
                        prune_expired(&mut state, now_ms()?)
                    };
                    if expired {
                        self.shared.notify_changed();
                    }
                    if expired
                        && let Some(started) = active.as_mut()
                        && let Err(error) = self.refresh_discovery(started).await
                    {
                        self.shared.state.lock().await.available = false;
                        self.shared.notify_changed();
                        eprintln!("runtime-host: device discovery degraded: {error}");
                    }
                }
                changed = self.speech_status.changed(), if speech_status_open => {
                    speech_status_open = changed.is_ok();
                    self.shared.notify_changed();
                }
                result = wait_server(&mut active), if active.is_some() => {
                    let error = result.unwrap_or_else(|| "device listener stopped".to_owned());
                    if let Some(started) = active.take() {
                        self.cleanup_stopped_gateway(started).await;
                    }
                    self.shared.state.lock().await.available = false;
                    return Err(DeviceGatewayError::Listener(error));
                }
            }
        }
        if let Some(started) = active.take() {
            self.stop_gateway(started).await;
        }
        self.mark_disabled().await;
        Ok(())
    }

    async fn start_gateway(&self) -> Result<ActiveGateway, DeviceGatewayError> {
        self.shared.recognition_tasks.reopen();
        let runtime_home = self.runtime_home.clone();
        let identity = tokio::task::spawn_blocking(move || {
            InstallationIdentity::load_or_create(&runtime_home)
        })
        .await
        .map_err(|_| DeviceGatewayError::IdentityTask)??;
        let identity = Arc::new(identity);
        let listener =
            std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
                .map_err(DeviceGatewayError::Bind)?;
        listener
            .set_nonblocking(true)
            .map_err(DeviceGatewayError::Bind)?;
        let port = listener
            .local_addr()
            .map_err(DeviceGatewayError::Bind)?
            .port();
        let tls = RustlsConfig::from_pem(
            identity.certificate_pem.clone(),
            identity.private_key_pem.clone(),
        )
        .await
        .map_err(DeviceGatewayError::Tls)?;
        let router = Router::new()
            .route("/device", get(super::connection::upgrade))
            .with_state(self.shared.clone());
        let server_handle = ServerHandle::new();
        let server = axum_server::from_tcp_rustls(listener, tls)
            .map_err(DeviceGatewayError::Bind)?
            .handle(server_handle.clone());
        let mdns = ServiceDaemon::new()
            .map_err(|error| DeviceGatewayError::Discovery(error.to_string()))?;
        let service_fullname = match self.register_discovery(&mdns, &identity, port).await {
            Ok(fullname) => fullname,
            Err(error) => {
                let _ = mdns.shutdown();
                return Err(error);
            }
        };
        {
            let mut state = self.shared.state.lock().await;
            state.enabled = true;
            state.available = true;
            state.identity = Some(identity);
        }
        self.shared.notify_changed();
        let server_task =
            tokio::spawn(async move { server.serve(router.into_make_service()).await });
        Ok(ActiveGateway {
            server_handle,
            server_task,
            mdns,
            service_fullname,
            port,
        })
    }

    async fn stop_gateway(&self, active: ActiveGateway) {
        self.disconnect_all().await;
        let _ = active.mdns.unregister(&active.service_fullname);
        let _ = active.mdns.shutdown();
        active
            .server_handle
            .graceful_shutdown(Some(Duration::from_secs(5)));
        let _ = active.server_task.await;
        self.stop_recognition_tasks().await;
    }

    async fn cleanup_stopped_gateway(&self, active: ActiveGateway) {
        self.disconnect_all().await;
        let _ = active.mdns.unregister(&active.service_fullname);
        let _ = active.mdns.shutdown();
        self.stop_recognition_tasks().await;
    }

    async fn refresh_discovery(
        &self,
        active: &mut ActiveGateway,
    ) -> Result<(), DeviceGatewayError> {
        let identity = self
            .shared
            .state
            .lock()
            .await
            .identity
            .clone()
            .ok_or(DeviceGatewayError::Unavailable)?;
        let _ = active.mdns.unregister(&active.service_fullname);
        active.service_fullname = self
            .register_discovery(&active.mdns, &identity, active.port)
            .await?;
        Ok(())
    }

    async fn register_discovery(
        &self,
        mdns: &ServiceDaemon,
        identity: &InstallationIdentity,
        port: u16,
    ) -> Result<String, DeviceGatewayError> {
        let pairing_available = self.shared.pairing_is_open(now_ms()?).await;
        let properties = [
            (
                "protocol_major",
                super::protocol::PROTOCOL_MAJOR.to_string(),
            ),
            (
                "protocol_minor",
                super::protocol::PROTOCOL_MINOR.to_string(),
            ),
            ("path", "/device".to_owned()),
            ("installation_id", identity.installation_id.clone()),
            (
                "certificate_fingerprint",
                identity.certificate_fingerprint.clone(),
            ),
            ("pairing_available", pairing_available.to_string()),
        ];
        let hostname = format!("{}.local.", identity.installation_id);
        let service = ServiceInfo::new(
            DEVICE_SERVICE_TYPE,
            &identity.installation_id,
            &hostname,
            "",
            port,
            &properties[..],
        )
        .map_err(|error| DeviceGatewayError::Discovery(error.to_string()))?
        .enable_addr_auto();
        let fullname = service.get_fullname().to_owned();
        mdns.register(service)
            .map_err(|error| DeviceGatewayError::Discovery(error.to_string()))?;
        Ok(fullname)
    }

    async fn disconnect_all(&self) {
        let connections = {
            let mut state = self.shared.state.lock().await;
            state
                .connections
                .drain()
                .map(|(_, connection)| connection.command)
                .collect::<Vec<_>>()
        };
        for connection in connections {
            let _ = connection.try_send(ConnectionCommand::GatewayDisabled);
        }
    }

    /// 在 listener 已停止接受新连接后，取消并等待 Gateway 拥有的全部 ASR 子任务。
    ///
    /// 先从按设备聚合表移除所有易失轮次，再发出每段取消信号；任务完成结果只能写入各自仍持有的
    /// 旧聚合 `Arc`，不会重新进入新的 Gateway 周期。`TaskTracker` 关闭后等待全部 wrapper 终态，
    /// 不提交 Runtime Input，也不触碰 SpeechService 自己拥有的 Provider 任务集合。
    async fn stop_recognition_tasks(&self) {
        let voice_turns = {
            let mut voice_turns = self.shared.voice_turns.lock().await;
            std::mem::take(&mut *voice_turns)
        };
        for voice_turn in voice_turns.into_values() {
            if let Some(aggregation) = voice_turn.lock().await.as_mut() {
                aggregation.cancel();
            }
        }
        self.shared.recognition_tasks.close();
        self.shared.recognition_tasks.wait().await;
    }

    async fn mark_disabled(&self) {
        let mut state = self.shared.state.lock().await;
        state.enabled = false;
        state.available = false;
        state.pairing_window_expires_at_ms = None;
        state.pending_pairings.clear();
        drop(state);
        self.shared.notify_changed();
    }
}

impl DeviceGatewayHandle {
    pub(crate) async fn set_enabled(&self, enabled: bool) -> Result<(), DeviceGatewayError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.lifecycle
            .send(LifecycleCommand::SetEnabled {
                enabled,
                response: response_tx,
            })
            .await
            .map_err(|_| DeviceGatewayError::Unavailable)?;
        response_rx
            .await
            .map_err(|_| DeviceGatewayError::Unavailable)?
    }

    pub(crate) async fn open_pairing_window(&self) -> Result<(), DeviceGatewayError> {
        let expires_at_ms = now_ms()?
            .checked_add(i64::try_from(PAIRING_WINDOW_DURATION.as_millis()).unwrap_or(i64::MAX))
            .ok_or(DeviceGatewayError::Clock)?;
        let mut state = self.shared.state.lock().await;
        if !state.enabled || !state.available {
            return Err(DeviceGatewayError::Unavailable);
        }
        state.pairing_window_expires_at_ms = Some(expires_at_ms);
        drop(state);
        self.shared.notify_changed();
        let _ = self.lifecycle.try_send(LifecycleCommand::RefreshDiscovery);
        Ok(())
    }

    pub(crate) async fn close_pairing_window(&self) {
        let mut state = self.shared.state.lock().await;
        state.pairing_window_expires_at_ms = None;
        state.pending_pairings.clear();
        drop(state);
        self.shared.notify_changed();
        let _ = self.lifecycle.try_send(LifecycleCommand::RefreshDiscovery);
    }

    pub(crate) async fn confirm_pairing(
        &self,
        request: ConfirmDevicePairingRequest,
    ) -> Result<(), DeviceGatewayError> {
        let decision = {
            let mut state = self.shared.state.lock().await;
            prune_expired(&mut state, now_ms()?);
            let pending = state
                .pending_pairings
                .get_mut(&request.pairing_request_id)
                .ok_or(DeviceGatewayError::PairingNotFound)?;
            if pending.remaining_attempts == 0 {
                return Err(DeviceGatewayError::PairingAttemptsExhausted);
            }
            pending.remaining_attempts -= 1;
            (
                pending.decision.clone(),
                PairingDecision {
                    pairing_code: request.pairing_code.expose().to_owned(),
                    display_name: request.display_name,
                },
            )
        };
        self.shared.notify_changed();
        decision
            .0
            .send(decision.1)
            .await
            .map_err(|_| DeviceGatewayError::PairingDisconnected)
    }

    pub(crate) async fn snapshot(&self) -> Result<DeviceGatewaySnapshot, DeviceGatewayError> {
        let now = now_ms()?;
        let devices = self
            .shared
            .runtime
            .registered_devices()
            .map_err(DeviceGatewayError::Runtime)?;
        let mut state = self.shared.state.lock().await;
        prune_expired(&mut state, now);
        let mut summaries = devices
            .into_iter()
            .map(|device| DeviceSummarySnapshot {
                connection: state.connections.get(&device.device_id).map(|connection| {
                    DeviceConnectionSnapshot {
                        connected_at_ms: connection.connected_at_ms,
                        capabilities: connection.capabilities,
                        output_preference: connection.output_preference,
                    }
                }),
                device_id: device.device_id,
                display_name: device.display_name,
                lifecycle: match device.lifecycle {
                    DeviceLifecycle::Paired => DeviceLifecycleSnapshot::Paired,
                    DeviceLifecycle::Revoked => DeviceLifecycleSnapshot::Revoked,
                },
                paired_at_ms: device.paired_at_ms,
                updated_at_ms: device.updated_at_ms,
                revoked_at_ms: device.revoked_at_ms,
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        let mut pending_pairings = state
            .pending_pairings
            .iter()
            .map(
                |(pairing_request_id, pending)| PendingDevicePairingSnapshot {
                    pairing_request_id: pairing_request_id.clone(),
                    display_name: pending.display_name.clone(),
                    capabilities: pending.capabilities,
                    expires_at_ms: pending.expires_at_ms,
                    remaining_attempts: pending.remaining_attempts,
                },
            )
            .collect::<Vec<_>>();
        pending_pairings
            .sort_by(|left, right| left.pairing_request_id.cmp(&right.pairing_request_id));
        Ok(DeviceGatewaySnapshot {
            enabled: state.enabled,
            available: state.available,
            installation_id: state
                .identity
                .as_ref()
                .map_or_else(String::new, |identity| identity.installation_id.clone()),
            certificate_fingerprint: state
                .identity
                .as_ref()
                .map_or_else(String::new, |identity| {
                    identity.certificate_fingerprint.clone()
                }),
            pairing_window: state
                .pairing_window_expires_at_ms
                .map(|expires_at_ms| DevicePairingWindowSnapshot { expires_at_ms }),
            pending_pairings,
            devices: summaries,
            speech_services: self.shared.speech.status(),
        })
    }

    pub(crate) async fn revoke_connection(&self, device_id: &assistant_protocol::DeviceId) {
        if let Some(connection) = self.shared.state.lock().await.connections.remove(device_id) {
            let _ = connection.command.try_send(ConnectionCommand::Revoked);
            self.shared.notify_changed();
        }
        let voice_turn = self.shared.voice_turns.lock().await.remove(device_id);
        if let Some(voice_turn) = voice_turn
            && let Some(aggregation) = voice_turn.lock().await.as_mut()
        {
            aggregation.cancel();
        }
    }

    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<DeviceGatewayEvent> {
        self.shared.events.subscribe()
    }

    pub(crate) fn notify_changed(&self) {
        self.shared.notify_changed();
    }
}

impl GatewayShared {
    pub(super) async fn voice_turn(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> Arc<Mutex<Option<super::connection::VoiceTurnAggregation>>> {
        self.voice_turns
            .lock()
            .await
            .entry(device_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(None)))
            .clone()
    }

    pub(super) async fn installation_identity(
        &self,
    ) -> Result<Arc<InstallationIdentity>, DeviceGatewayError> {
        self.state
            .lock()
            .await
            .identity
            .clone()
            .ok_or(DeviceGatewayError::Unavailable)
    }

    pub(super) async fn pairing_is_open(&self, now: i64) -> bool {
        let mut state = self.state.lock().await;
        prune_expired(&mut state, now);
        state.enabled
            && state.available
            && state
                .pairing_window_expires_at_ms
                .is_some_and(|expires_at| expires_at > now)
    }

    pub(super) async fn register_pending(
        &self,
        request_id: String,
        display_name: String,
        capabilities: DeviceCapabilitiesSnapshot,
        decision: mpsc::Sender<PairingDecision>,
    ) -> Result<i64, DeviceGatewayError> {
        let now = now_ms()?;
        let mut state = self.state.lock().await;
        prune_expired(&mut state, now);
        let window_expires = state
            .pairing_window_expires_at_ms
            .filter(|expires_at| *expires_at > now)
            .ok_or(DeviceGatewayError::PairingNotOpen)?;
        let pending = state
            .pending_pairings
            .entry(request_id)
            .or_insert_with(|| PendingPairing {
                display_name: display_name.clone(),
                capabilities,
                expires_at_ms: window_expires,
                remaining_attempts: MAX_PAIRING_ATTEMPTS,
                decision: decision.clone(),
            });
        if pending.remaining_attempts == 0 {
            return Err(DeviceGatewayError::PairingAttemptsExhausted);
        }
        pending.display_name = display_name;
        pending.capabilities = capabilities;
        pending.decision = decision;
        let expires_at_ms = pending.expires_at_ms;
        drop(state);
        self.notify_changed();
        Ok(expires_at_ms)
    }

    pub(super) async fn remove_pending(&self, request_id: &str) {
        if self
            .state
            .lock()
            .await
            .pending_pairings
            .remove(request_id)
            .is_some()
        {
            self.notify_changed();
        }
    }

    pub(super) async fn register_connection(
        &self,
        device_id: assistant_protocol::DeviceId,
        connection_id: String,
        capabilities: DeviceCapabilitiesSnapshot,
        output_preference: OutputPreferenceSnapshot,
    ) -> mpsc::Receiver<ConnectionCommand> {
        let (command_tx, command_rx) = mpsc::channel(CONNECTION_COMMAND_CAPACITY);
        let previous = self.state.lock().await.connections.insert(
            device_id,
            ActiveConnection {
                connection_id,
                connected_at_ms: now_ms().unwrap_or_default(),
                capabilities,
                output_preference,
                command: command_tx,
            },
        );
        if let Some(previous) = previous {
            let _ = previous.command.try_send(ConnectionCommand::Replaced);
        }
        self.notify_changed();
        command_rx
    }

    pub(super) async fn update_preference(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        preference: OutputPreferenceSnapshot,
    ) {
        let changed = {
            let mut state = self.state.lock().await;
            if let Some(connection) = state.connections.get_mut(device_id)
                && connection.connection_id == connection_id
                && connection.output_preference != preference
            {
                connection.output_preference = preference;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify_changed();
        }
    }

    pub(super) async fn unregister_connection(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
    ) {
        let mut state = self.state.lock().await;
        if state
            .connections
            .get(device_id)
            .is_some_and(|connection| connection.connection_id == connection_id)
        {
            state.connections.remove(device_id);
            drop(state);
            self.notify_changed();
        }
    }

    fn notify_changed(&self) {
        let _ = self.events.send(DeviceGatewayEvent::Changed);
    }

    /// 规范 Run 已结算后，把每条附加 delivery 独立解析到当前认证连接。
    /// 这里不持久化正文或连接 ID；任何终端失败只返回分发错误，不反向修改 Runtime 事实。
    pub(super) async fn dispatch_channel_output(
        &self,
        output: ChannelOutput,
    ) -> Result<(), ChannelOutputDispatchError> {
        let output_id =
            super::crypto::random_token(12).map_err(|_| ChannelOutputDispatchError::Unavailable)?;
        let mut failed = false;
        for delivery in &output.deliveries {
            let ResolvedChannelDelivery::Device {
                device_id,
                preference,
            } = delivery;
            let target = {
                let state = self.state.lock().await;
                let Some(connection) = state.connections.get(device_id) else {
                    log_delivery_failure(&output, device_id, "device_offline");
                    failed = true;
                    continue;
                };
                (
                    connection.command.clone(),
                    connection.capabilities,
                    connection.output_preference,
                )
            };
            let requested = resolve_requested_preference(target.2, preference.clone());
            if !preference_is_supported(target.1, output_preference_to_snapshot(requested)) {
                let _ = target
                    .0
                    .try_send(ConnectionCommand::OutputUnavailable(unavailable_state(
                        &output,
                        "unsupported_output_preference",
                    )));
                log_delivery_failure(&output, device_id, "output_unavailable");
                failed = true;
                continue;
            }
            if matches!(
                requested,
                OutputPreference::Text | OutputPreference::TextAndAudio
            ) {
                let command = match output.assistant_text.as_ref() {
                    Some(text) => ConnectionCommand::TextOutput(TextOutput {
                        output_id: output_id.clone(),
                        run_id: output.run_id.clone(),
                        text: text.clone(),
                    }),
                    None => ConnectionCommand::OutputUnavailable(unavailable_state(
                        &output,
                        "assistant_text_unavailable",
                    )),
                };
                if target.0.try_send(command).is_err() {
                    log_delivery_failure(&output, device_id, "connection_backpressure");
                    failed = true;
                    continue;
                }
            }
            if !matches!(
                requested,
                OutputPreference::Audio | OutputPreference::TextAndAudio
            ) {
                continue;
            }
            if !output.speech_completed {
                let _ = target
                    .0
                    .try_send(ConnectionCommand::OutputUnavailable(unavailable_state(
                        &output,
                        "no_speak_text",
                    )));
                log_delivery_failure(&output, device_id, "no_speak_text");
                failed = true;
            }
        }
        if failed {
            Err(ChannelOutputDispatchError::Unavailable)
        } else {
            Ok(())
        }
    }

    pub(super) async fn requires_speech(&self, deliveries: &[ResolvedChannelDelivery]) -> bool {
        if !self.speech.tts_available() {
            return false;
        }
        let state = self.state.lock().await;
        deliveries.iter().any(|delivery| {
            let ResolvedChannelDelivery::Device {
                device_id,
                preference,
            } = delivery;
            let Some(connection) = state.connections.get(device_id) else {
                return false;
            };
            let requested =
                resolve_requested_preference(connection.output_preference, preference.clone());
            matches!(
                requested,
                OutputPreference::Audio | OutputPreference::TextAndAudio
            ) && preference_is_supported(
                connection.capabilities,
                output_preference_to_snapshot(requested),
            )
        })
    }

    /// `speak` 已可靠进入 Tool 执行后立即合成一个独立片段，并按连接预留顺序入队。
    /// 播放队列属于连接易失状态；失败只反馈当前 Tool，不回写 Run 或 Conversation。
    pub(super) async fn dispatch_speech_segment(
        &self,
        segment: ChannelSpeechSegment,
    ) -> Result<(), ChannelOutputDispatchError> {
        if !self.speech.tts_available() {
            return Err(ChannelOutputDispatchError::Unavailable);
        }
        let output_id = format!("{}-{}", segment.run_id.as_str(), segment.segment_id);
        eprintln!(
            "event=speech_delivery_started ts_ms={} request={} run={} targets={}",
            crate::media_diagnostics::timestamp_ms(),
            crate::media_diagnostics::correlation_id(&output_id),
            crate::media_diagnostics::correlation_id(segment.run_id.as_str()),
            segment.deliveries.len()
        );
        let mut targets = Vec::new();
        let mut failed = false;
        let mut interrupted = false;
        for delivery in &segment.deliveries {
            let ResolvedChannelDelivery::Device {
                device_id,
                preference,
            } = delivery;
            let target = {
                let state = self.state.lock().await;
                let Some(connection) = state.connections.get(device_id) else {
                    failed = true;
                    continue;
                };
                (
                    connection.command.clone(),
                    connection.capabilities,
                    connection.output_preference,
                )
            };
            let requested = resolve_requested_preference(target.2, preference.clone());
            if !matches!(
                requested,
                OutputPreference::Audio | OutputPreference::TextAndAudio
            ) {
                continue;
            }
            if !preference_is_supported(target.1, output_preference_to_snapshot(requested)) {
                failed = true;
                continue;
            }
            match PreparedPlayback::reserve(
                target.0,
                output_id.clone(),
                segment.cancellation.child_token(),
            )
            .await
            {
                Ok(target) => targets.push(target),
                Err(ChannelOutputDispatchError::Cancelled) => interrupted = true,
                Err(_) => failed = true,
            }
        }
        if targets.is_empty() {
            return Err(if interrupted {
                ChannelOutputDispatchError::Cancelled
            } else {
                ChannelOutputDispatchError::Unavailable
            });
        }
        let debug_name = format!("{}-{}", segment.run_id.as_str(), segment.segment_id);
        // 与调用方 Future 同生共死，不再创建无人等待的取消监视任务；丢弃合成 Future 由
        // SpeechService 的 response.closed 收口。任一未交付预留在退出时自动取消。
        let synthesized = tokio::select! {
            result = self.speech.synthesize(segment.text.clone(), debug_name, segment.cancellation.child_token()) => result,
            () = async { for target in &targets { target.cancellation().cancelled().await; } } => Err(SpeechServiceError::Cancelled),
        };
        match synthesized {
            Ok(pcm) => {
                for target in targets {
                    match target
                        .attach(PlaybackOutput {
                            output_id: output_id.clone(),
                            run_id: segment.run_id.clone(),
                            text: segment.text.clone(),
                            pcm: pcm.clone(),
                        })
                        .await
                    {
                        Ok(()) => {}
                        Err(ChannelOutputDispatchError::Cancelled) => interrupted = true,
                        Err(_) => failed = true,
                    }
                }
            }
            Err(SpeechServiceError::Cancelled) => {
                return Err(ChannelOutputDispatchError::Cancelled);
            }
            Err(_) => {
                for target in targets {
                    target.notify_unavailable(segment_unavailable_state(
                        &segment,
                        "tts_provider_failed",
                    ));
                }
                return Err(ChannelOutputDispatchError::Unavailable);
            }
        }
        if failed {
            Err(ChannelOutputDispatchError::Unavailable)
        } else if interrupted {
            Err(ChannelOutputDispatchError::Cancelled)
        } else {
            Ok(())
        }
    }
}

fn segment_unavailable_state(
    segment: &ChannelSpeechSegment,
    reason: &str,
) -> InteractionStateChanged {
    InteractionStateChanged {
        run_id: Some(segment.run_id.clone()),
        client_input_id: None,
        state: "unavailable".to_owned(),
        reason: Some(reason.to_owned()),
    }
}

fn log_delivery_failure(
    output: &ChannelOutput,
    device_id: &assistant_protocol::DeviceId,
    reason: &str,
) {
    let device_short = device_id.as_str().chars().take(12).collect::<String>();
    eprintln!(
        "runtime-host: device delivery failed: run={} device={} reason={reason}",
        output.run_id.as_str(),
        device_short,
    );
}

fn unavailable_state(output: &ChannelOutput, reason: &str) -> InteractionStateChanged {
    InteractionStateChanged {
        run_id: Some(output.run_id.clone()),
        client_input_id: None,
        state: "unavailable".to_owned(),
        reason: Some(reason.to_owned()),
    }
}

fn output_preference_from_snapshot(preference: OutputPreferenceSnapshot) -> OutputPreference {
    match preference {
        OutputPreferenceSnapshot::Text => OutputPreference::Text,
        OutputPreferenceSnapshot::Audio => OutputPreference::Audio,
        OutputPreferenceSnapshot::TextAndAudio => OutputPreference::TextAndAudio,
    }
}

fn resolve_requested_preference(
    preset: OutputPreferenceSnapshot,
    preference: DeviceDeliveryPreference,
) -> OutputPreference {
    match preference {
        DeviceDeliveryPreference::Frozen(preference) => preference,
        DeviceDeliveryPreference::Preset => output_preference_from_snapshot(preset),
    }
}

fn output_preference_to_snapshot(preference: OutputPreference) -> OutputPreferenceSnapshot {
    match preference {
        OutputPreference::Text => OutputPreferenceSnapshot::Text,
        OutputPreference::Audio => OutputPreferenceSnapshot::Audio,
        OutputPreference::TextAndAudio => OutputPreferenceSnapshot::TextAndAudio,
    }
}

fn prune_expired(state: &mut GatewayState, now: i64) -> bool {
    if state
        .pairing_window_expires_at_ms
        .is_some_and(|expires_at| expires_at <= now)
    {
        state.pairing_window_expires_at_ms = None;
        state.pending_pairings.clear();
        true
    } else {
        state
            .pending_pairings
            .retain(|_, pending| pending.expires_at_ms > now && pending.remaining_attempts > 0);
        false
    }
}

async fn wait_server(active: &mut Option<ActiveGateway>) -> Option<String> {
    let active = active.as_mut()?;
    match (&mut active.server_task).await {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(error) => Some(if error.is_panic() {
            "device listener task panicked".to_owned()
        } else {
            "device listener task was cancelled".to_owned()
        }),
    }
}

fn now_ms() -> Result<i64, DeviceGatewayError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| DeviceGatewayError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| DeviceGatewayError::Clock)
}

/// Device Gateway 对 Host 管理入口暴露的稳定错误分类。
///
/// 转换为协议错误时会隐藏证书、网络和内部 Runtime 细节。
#[derive(Debug, Error)]
pub(crate) enum DeviceGatewayError {
    #[error("device gateway is unavailable")]
    Unavailable,
    #[error("device pairing window is not open")]
    PairingNotOpen,
    #[error("device pairing request was not found")]
    PairingNotFound,
    #[error("device pairing attempts are exhausted")]
    PairingAttemptsExhausted,
    #[error("device pairing connection ended")]
    PairingDisconnected,
    #[error("system clock is unavailable")]
    Clock,
    #[error("device installation identity task failed")]
    IdentityTask,
    #[error("device installation identity failed: {0}")]
    Identity(#[from] IdentityError),
    #[error("device listener could not bind: {0}")]
    Bind(std::io::Error),
    #[error("device TLS configuration failed: {0}")]
    Tls(std::io::Error),
    #[error("device discovery failed: {0}")]
    Discovery(String),
    #[error("device listener failed: {0}")]
    Listener(String),
    #[error("runtime device operation failed: {0}")]
    Runtime(assistant_runtime::RuntimeError),
}

impl DeviceGatewayError {
    pub(crate) fn to_protocol_info(&self) -> RuntimeErrorInfo {
        match self {
            Self::PairingNotOpen => RuntimeErrorInfo::new(
                RuntimeErrorCode::OperationNotAllowed,
                "device pairing window is not open",
            ),
            Self::PairingNotFound => RuntimeErrorInfo::new(
                RuntimeErrorCode::InvalidRequest,
                "device pairing request was not found",
            ),
            Self::PairingAttemptsExhausted => RuntimeErrorInfo::new(
                RuntimeErrorCode::OperationNotAllowed,
                "device pairing attempts are exhausted",
            ),
            Self::PairingDisconnected => RuntimeErrorInfo::new(
                RuntimeErrorCode::Conflict,
                "device pairing connection ended; wait for the device to reconnect",
            ),
            Self::Runtime(error) => error.to_protocol_info(),
            Self::Unavailable
            | Self::Clock
            | Self::IdentityTask
            | Self::Identity(_)
            | Self::Bind(_)
            | Self::Tls(_)
            | Self::Discovery(_)
            | Self::Listener(_) => {
                RuntimeErrorInfo::new(RuntimeErrorCode::Internal, "device gateway is unavailable")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use assistant_runtime::DeviceDeliveryPreference;

    use super::*;

    #[test]
    fn frozen_source_and_hosting_preset_resolve_at_the_correct_boundary() {
        assert_eq!(
            resolve_requested_preference(
                OutputPreferenceSnapshot::Audio,
                DeviceDeliveryPreference::Frozen(OutputPreference::Text),
            ),
            OutputPreference::Text
        );
        assert_eq!(
            resolve_requested_preference(
                OutputPreferenceSnapshot::Audio,
                DeviceDeliveryPreference::Preset,
            ),
            OutputPreference::Audio
        );
    }
}
