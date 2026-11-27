//! Device Gateway 的进程内所有权、启停、发现与管理投影。

mod playback;

use playback::PreparedPlayback;

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    ConfirmDevicePairingRequest, ConnectDiscoveredDeviceRequest, DeviceCapabilitiesSnapshot,
    DeviceConnectionSnapshot, DeviceGatewayEvent, DeviceGatewaySnapshot, DeviceLifecycleSnapshot,
    DevicePairingModeSnapshot, DevicePairingWindowSnapshot, DeviceSpeechServicesSnapshot,
    DeviceSummarySnapshot, DiscoveredDeviceSnapshot, McpServerKey, OutputPreferenceSnapshot,
    PendingDevicePairingSnapshot, RejectDevicePairingRequest, RuntimeErrorCode, RuntimeErrorInfo,
};
use assistant_runtime::{
    AssistantRuntime, ChannelOutput, ChannelOutputDispatchError, ChannelSpeechSegment,
    DeviceDeliveryPreference, DeviceLifecycle, OutputPreference, ResolvedChannelDelivery,
};
use axum::{Router, routing::{get, post}};
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
    console::DeviceConsoleTarget,
    dispatcher::DeviceChannelOutputDispatcher,
    native_console::NativeConsoleStore,
    protocol::{
        DeviceCommandRequest, DeviceCommandResult, DeviceHardwareDeclaration,
        InteractionStateChanged, SpeechAsrResult, TextOutput, VoiceprintSyncRequest,
        preference_is_supported,
    },
};
use crate::speech::{SpeechServiceError, SpeechServiceHandle};

const PAIRING_WINDOW_DURATION: Duration = Duration::from_secs(5 * 60);
const TARGETED_PAIRING_TTL: Duration = Duration::from_secs(60);
pub(super) const MAX_PAIRING_ATTEMPTS: u8 = 5;
const LIFECYCLE_QUEUE_CAPACITY: usize = 16;
const CONNECTION_COMMAND_CAPACITY: usize = 32;
const GATEWAY_EVENT_CAPACITY: usize = 32;
const DEVICE_SERVICE_TYPE: &str = "_ez-assistant._tcp.local.";
const DISCOVERABLE_DEVICE_SERVICE_TYPE: &str = "_ez-device._tcp.local.";
const GATEWAY_STATE_DIRECTORY: &str = "device";
const GATEWAY_STATE_FILE: &str = "gateway.json";

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct GatewayPreferenceDocument {
    schema_version: u32,
    enabled: bool,
}

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
    pub(super) native_console: Arc<NativeConsoleStore>,
    state: Mutex<GatewayState>,
    voice_turns: Mutex<
        HashMap<
            assistant_protocol::DeviceId,
            Arc<Mutex<Option<super::connection::VoiceTurnAggregation>>>,
        >,
    >,
    mcp_locks: Mutex<HashMap<assistant_protocol::DeviceId, Arc<Mutex<()>>>>,
    pub(super) recognition_tasks: TaskTracker,
    pub(super) mcp_tasks: TaskTracker,
    next_pending_token: AtomicU64,
    events: broadcast::Sender<DeviceGatewayEvent>,
}

/// Gateway 当前进程内状态；关闭 Host 后全部失效。
struct GatewayState {
    enabled: bool,
    available: bool,
    active_port: Option<u16>,
    active_mcp_port: Option<u16>,
    active_mcp_token: Option<String>,
    identity: Option<Arc<InstallationIdentity>>,
    pairing_window_expires_at_ms: Option<i64>,
    pending_pairings: HashMap<String, PendingPairing>,
    discovered_devices: HashMap<String, DiscoveredDeviceRecord>,
    targeted_auto_approves: HashMap<String, TargetedAutoApprove>,
    connections: HashMap<assistant_protocol::DeviceId, ActiveConnection>,
}

/// 局域网 mDNS 浏览发现的未配对终端记录。
#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveredDeviceRecord {
    discovery_id: String,
    display_name: String,
    model: String,
    peer_ip: IpAddr,
    control_port: u16,
    key_fingerprint: String,
    firmware_version: Option<String>,
    mac_address: Option<String>,
}

/// 点击「连接」后授予特定密钥指纹 + IP 的一次性 60 秒自动配对许可。
#[derive(Clone, Debug)]
struct TargetedAutoApprove {
    peer_ip: IpAddr,
    display_name: Option<String>,
    expires_at_ms: i64,
}

/// 等待 Desktop 确认（输入配对码或一键确认）的候选设备及其一次性决策通道。
pub(super) struct PendingPairing {
    pub(super) display_name: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    pub(super) expires_at_ms: i64,
    pub(super) remaining_attempts: u8,
    pub(super) mode: DevicePairingModeSnapshot,
    pub(super) peer_address: Option<String>,
    pub(super) key_fingerprint: Option<String>,
    pub(super) client_version: Option<String>,
    pub(super) token: u64,
    pub(super) decision: mpsc::Sender<PairingDecision>,
}

/// 注册待确认配对候选时的元数据输入。
pub(super) struct PendingPairingRegistration {
    pub(super) request_id: String,
    pub(super) display_name: String,
    pub(super) capabilities: DeviceCapabilitiesSnapshot,
    pub(super) mode: DevicePairingModeSnapshot,
    pub(super) remaining_attempts: u8,
    pub(super) peer_address: Option<String>,
    pub(super) key_fingerprint: Option<String>,
    pub(super) client_version: Option<String>,
    pub(super) decision: mpsc::Sender<PairingDecision>,
}

/// Desktop 对候选设备作出的配对确认或拒绝，不代表设备身份已经持久化。
pub(super) enum PairingDecision {
    ApproveSpake2 {
        pairing_code: String,
        display_name: Option<String>,
    },
    ApproveDirect {
        display_name: Option<String>,
    },
    Reject,
}

/// 一台已认证设备的当前唯一在线连接。
///
/// 新连接替换旧连接时通过 `command` 通知旧连接收敛，避免同一设备并行接收输出。
struct ActiveConnection {
    connection_id: String,
    connected_at_ms: i64,
    capabilities: DeviceCapabilitiesSnapshot,
    output_preference: OutputPreferenceSnapshot,
    peer_address: Option<String>,
    client_version: Option<String>,
    mcp_server_keys: Vec<McpServerKey>,
    console: Option<DeviceConsoleTarget>,
    hardware: Option<DeviceHardwareDeclaration>,
    active_speaker_role: i32,
    speech_relay: bool,
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
    InterruptPlayback {
        reason: String,
    },
    SpeechStatusChanged(DeviceSpeechServicesSnapshot),
    SpeechAsrResult(SpeechAsrResult),
    SpeechTtsResult {
        request_id: String,
        stream_id: u32,
        result: Result<Arc<[u8]>, SpeechServiceError>,
    },
    ExecuteDeviceCommand {
        request: DeviceCommandRequest,
        response: oneshot::Sender<DeviceCommandResult>,
    },
    SyncVoiceprints(VoiceprintSyncRequest),
    SyncWakeConfig {
        request: super::protocol::WakeConfigSyncRequest,
        response: Option<oneshot::Sender<super::protocol::WakeConfigAck>>,
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
    mcp_server_handle: ServerHandle<SocketAddr>,
    mcp_server_task: JoinHandle<Result<(), std::io::Error>>,
    mdns: ServiceDaemon,
    service_fullname: String,
    port: u16,
    advertised_pairing_available: bool,
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
        let native_console = Arc::new(NativeConsoleStore::load_or_default(&runtime_home));
        let shared = Arc::new(GatewayShared {
            runtime,
            speech,
            native_console,
            events: event_tx,
            voice_turns: Mutex::new(HashMap::new()),
            mcp_locks: Mutex::new(HashMap::new()),
            recognition_tasks: TaskTracker::new(),
            mcp_tasks: TaskTracker::new(),
            next_pending_token: AtomicU64::new(1),
            state: Mutex::new(GatewayState {
                enabled: false,
                available: false,
                active_port: None,
                active_mcp_port: None,
                active_mcp_token: None,
                identity: None,
                pairing_window_expires_at_ms: None,
                pending_pairings: HashMap::new(),
                discovered_devices: HashMap::new(),
                targeted_auto_approves: HashMap::new(),
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
        let should_auto_enable = match load_gateway_enabled_preference(&self.runtime_home) {
            Some(enabled) => enabled,
            None => self.shared.runtime.registered_devices().is_ok_and(|devices| {
                devices
                    .iter()
                    .any(|device| device.lifecycle == DeviceLifecycle::Paired)
            }),
        };
        if should_auto_enable {
            match self.start_gateway().await {
                Ok(started) => active = Some(started),
                Err(error) => {
                    eprintln!("runtime-host: device gateway auto-start degraded: {error}");
                }
            }
        }
        let (browser_mdns, mut browser_rx) = match ServiceDaemon::new().and_then(|daemon| {
            daemon
                .browse(DISCOVERABLE_DEVICE_SERVICE_TYPE)
                .map(|rx| (daemon, rx))
        }) {
            Ok((daemon, rx)) => (Some(daemon), Some(rx)),
            Err(error) => {
                eprintln!("runtime-host: discovered-device mDNS browser unavailable: {error}");
                (None, None)
            }
        };
        let mut result = Ok(());
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                command = self.lifecycle.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    match command {
                        LifecycleCommand::SetEnabled { enabled, response } => {
                            let res = if enabled {
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
                            if res.is_ok()
                                && let Err(error) =
                                    save_gateway_enabled_preference(&self.runtime_home, enabled)
                            {
                                eprintln!(
                                    "runtime-host: failed to persist device gateway preference: {error}"
                                );
                            }
                            let _ = response.send(res);
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
                browser_event = recv_browser_event(&mut browser_rx), if browser_rx.is_some() => {
                    match browser_event {
                        Some(event) => self.handle_discovery_event(event).await,
                        None => browser_rx = None,
                    }
                }
                _ = pairing_expiry.tick() => {
                    let (expired, currently_open) = {
                        let mut state = self.shared.state.lock().await;
                        let now = now_ms()?;
                        let expired = prune_expired(&mut state, now);
                        let open = state.enabled
                            && state.available
                            && state
                                .pairing_window_expires_at_ms
                                .is_some_and(|expires_at| expires_at > now);
                        (expired, open)
                    };
                    if expired {
                        self.shared.notify_changed();
                    }
                    if let Some(started) = active.as_mut()
                        && (expired || started.advertised_pairing_available != currently_open)
                        && let Err(error) = self.refresh_discovery(started).await
                    {
                        self.shared.state.lock().await.available = false;
                        self.shared.notify_changed();
                        eprintln!("runtime-host: device discovery degraded: {error}");
                    }
                }
                changed = self.speech_status.changed(), if speech_status_open => {
                    speech_status_open = changed.is_ok();
                    if speech_status_open {
                        let latest = *self.speech_status.borrow_and_update();
                        self.shared.broadcast_speech_status(latest).await;
                    }
                    self.shared.notify_changed();
                }
                wait_res = wait_server(&mut active), if active.is_some() => {
                    let error = wait_res.unwrap_or_else(|| "device listener stopped".to_owned());
                    if let Some(started) = active.take() {
                        self.cleanup_stopped_gateway(started).await;
                    }
                    self.shared.state.lock().await.available = false;
                    self.shared.state.lock().await.active_port = None;
                    result = Err(DeviceGatewayError::Listener(error));
                    break;
                }
            }
        }
        if let Some(daemon) = browser_mdns {
            let _ = daemon.shutdown();
        }
        if let Some(started) = active.take() {
            self.stop_gateway(started).await;
        }
        self.mark_disabled().await;
        result
    }

    async fn start_gateway(&self) -> Result<ActiveGateway, DeviceGatewayError> {
        self.shared.recognition_tasks.reopen();
        self.shared.mcp_tasks.reopen();
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
        crate::server::initialize_tls_provider();
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
        let (service_fullname, advertised_pairing_available) =
            match self.register_discovery(&mdns, &identity, port).await {
                Ok(registered) => registered,
                Err(error) => {
                    let _ = mdns.shutdown();
                    return Err(error);
                }
            };
        let mcp_listener =
            std::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
                .map_err(DeviceGatewayError::Bind)?;
        mcp_listener
            .set_nonblocking(true)
            .map_err(DeviceGatewayError::Bind)?;
        let mcp_port = mcp_listener
            .local_addr()
            .map_err(DeviceGatewayError::Bind)?
            .port();
        let mcp_token = super::crypto::random_token(32).map_err(|_| {
            DeviceGatewayError::Listener("failed to generate loopback MCP token".to_owned())
        })?;
        let mcp_router = Router::new()
            .route(
                "/mcp/{device_id}",
                post(super::native_console::handle_mcp_http_request),
            )
            .with_state(self.shared.clone());
        let mcp_server_handle = ServerHandle::new();
        let mcp_server = axum_server::from_tcp(mcp_listener)
            .map_err(DeviceGatewayError::Bind)?
            .handle(mcp_server_handle.clone());
        {
            let mut state = self.shared.state.lock().await;
            state.enabled = true;
            state.available = true;
            state.active_port = Some(port);
            state.active_mcp_port = Some(mcp_port);
            state.active_mcp_token = Some(mcp_token);
            state.identity = Some(identity);
        }
        self.shared.notify_changed();
        let server_task = tokio::spawn(async move {
            server
                .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                .await
        });
        let mcp_server_task = tokio::spawn(async move {
            mcp_server
                .serve(mcp_router.into_make_service())
                .await
        });
        Ok(ActiveGateway {
            server_handle,
            server_task,
            mcp_server_handle,
            mcp_server_task,
            mdns,
            service_fullname,
            port,
            advertised_pairing_available,
        })
    }

    async fn stop_gateway(&self, active: ActiveGateway) {
        let disconnected = self.disconnect_all().await;
        let _ = active.mdns.unregister(&active.service_fullname);
        let _ = active.mdns.shutdown();
        active
            .server_handle
            .graceful_shutdown(Some(Duration::from_secs(5)));
        active
            .mcp_server_handle
            .graceful_shutdown(Some(Duration::from_secs(2)));
        let _ = active.server_task.await;
        let _ = active.mcp_server_task.await;
        self.stop_recognition_tasks().await;
        self.stop_mcp_tasks(disconnected).await;
    }

    async fn cleanup_stopped_gateway(&self, active: ActiveGateway) {
        let disconnected = self.disconnect_all().await;
        let _ = active.mdns.unregister(&active.service_fullname);
        let _ = active.mdns.shutdown();
        active
            .mcp_server_handle
            .graceful_shutdown(Some(Duration::from_secs(2)));
        let _ = active.mcp_server_task.await;
        self.stop_recognition_tasks().await;
        self.stop_mcp_tasks(disconnected).await;
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
        let (service_fullname, advertised_pairing_available) = self
            .register_discovery(&active.mdns, &identity, active.port)
            .await?;
        active.service_fullname = service_fullname;
        active.advertised_pairing_available = advertised_pairing_available;
        Ok(())
    }

    async fn register_discovery(
        &self,
        mdns: &ServiceDaemon,
        identity: &InstallationIdentity,
        port: u16,
    ) -> Result<(String, bool), DeviceGatewayError> {
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
            ("pairing_modes", "direct_confirm,spake2".to_owned()),
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
        Ok((fullname, pairing_available))
    }

    async fn disconnect_all(&self) -> Vec<assistant_protocol::DeviceId> {
        let disconnected = {
            let mut state = self.shared.state.lock().await;
            state
                .connections
                .drain()
                .map(|(device_id, connection)| (device_id, connection.command))
                .collect::<Vec<_>>()
        };
        let mut device_ids = Vec::with_capacity(disconnected.len());
        for (device_id, connection) in disconnected {
            let _ = connection.try_send(ConnectionCommand::GatewayDisabled);
            device_ids.push(device_id);
        }
        device_ids
    }

    async fn stop_mcp_tasks(&self, disconnected: Vec<assistant_protocol::DeviceId>) {
        self.shared.mcp_tasks.close();
        self.shared.mcp_tasks.wait().await;
        for device_id in disconnected {
            let lock = self.shared.mcp_lock(&device_id).await;
            let _guard = lock.lock().await;
            if !self.shared.is_device_connected(&device_id).await {
                let _ = self
                    .shared
                    .runtime
                    .mark_device_mcp_offline(&device_id)
                    .await;
            }
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
        state.active_port = None;
        state.active_mcp_port = None;
        state.active_mcp_token = None;
        state.pairing_window_expires_at_ms = None;
        state.pending_pairings.clear();
        state.targeted_auto_approves.clear();
        drop(state);
        self.shared.notify_changed();
    }

    async fn handle_discovery_event(&self, event: mdns_sd::ServiceEvent) {
        match event {
            mdns_sd::ServiceEvent::ServiceResolved(info) => {
                let fullname = info.get_fullname().to_owned();
                let mut state = self.shared.state.lock().await;
                let changed = if let Some(record) = parse_discovered_record(&info) {
                    let same = state
                        .discovered_devices
                        .get(&fullname)
                        .is_some_and(|existing| *existing == record);
                    if same {
                        false
                    } else {
                        state.discovered_devices.insert(fullname, record);
                        true
                    }
                } else {
                    state.discovered_devices.remove(&fullname).is_some()
                };
                drop(state);
                if changed {
                    self.shared.notify_changed();
                }
            }
            mdns_sd::ServiceEvent::ServiceRemoved(_, fullname) => {
                let removed = self
                    .shared
                    .state
                    .lock()
                    .await
                    .discovered_devices
                    .remove(&fullname)
                    .is_some();
                if removed {
                    self.shared.notify_changed();
                }
            }
            mdns_sd::ServiceEvent::SearchStarted(_)
            | mdns_sd::ServiceEvent::ServiceFound(_, _)
            | mdns_sd::ServiceEvent::SearchStopped(_)
            | _ => {}
        }
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
            let payload = match pending.mode {
                DevicePairingModeSnapshot::Spake2 => {
                    let pairing_code = request
                        .pairing_code
                        .as_ref()
                        .map(|code| code.expose().trim().to_owned())
                        .unwrap_or_default();
                    if pairing_code.len() != 6
                        || !pairing_code.bytes().all(|byte| byte.is_ascii_digit())
                    {
                        return Err(DeviceGatewayError::InvalidPairingCode);
                    }
                    pending.remaining_attempts -= 1;
                    PairingDecision::ApproveSpake2 {
                        pairing_code,
                        display_name: request.display_name,
                    }
                }
                DevicePairingModeSnapshot::DirectConfirm => {
                    if request
                        .pairing_code
                        .as_ref()
                        .is_some_and(|code| !code.expose().trim().is_empty())
                    {
                        return Err(DeviceGatewayError::InvalidPairingCode);
                    }
                    pending.remaining_attempts = 0;
                    PairingDecision::ApproveDirect {
                        display_name: request.display_name,
                    }
                }
            };
            (pending.decision.clone(), payload)
        };
        self.shared.notify_changed();
        decision
            .0
            .send(decision.1)
            .await
            .map_err(|_| DeviceGatewayError::PairingDisconnected)
    }

    pub(crate) async fn reject_pairing(
        &self,
        request: RejectDevicePairingRequest,
    ) -> Result<(), DeviceGatewayError> {
        let decision = {
            let mut state = self.shared.state.lock().await;
            prune_expired(&mut state, now_ms()?);
            let pending = state
                .pending_pairings
                .remove(&request.pairing_request_id)
                .ok_or(DeviceGatewayError::PairingNotFound)?;
            pending.decision
        };
        self.shared.notify_changed();
        let _ = decision.try_send(PairingDecision::Reject);
        Ok(())
    }

    pub(crate) async fn connect_discovered_device(
        &self,
        request: ConnectDiscoveredDeviceRequest,
    ) -> Result<(), DeviceGatewayError> {
        let now = now_ms()?;
        let discovered = {
            let state = self.shared.state.lock().await;
            state
                .discovered_devices
                .get(&request.discovery_id)
                .cloned()
                .ok_or(DeviceGatewayError::PairingNotFound)?
        };

        let needs_enable = {
            let state = self.shared.state.lock().await;
            !state.enabled || !state.available
        };
        if needs_enable {
            self.set_enabled(true).await?;
        }

        let preferred_name = request
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| Some(discovered.display_name.clone()));

        // 若该设备已连入并在 pending_pairings 等待确认，点击「连接」时直接批准现有会话，
        // 避免固件停留在 kPairing 阻塞等待而忽略新的 HTTP /ez/connect 触发。
        let existing_pending = {
            let mut state = self.shared.state.lock().await;
            prune_expired(&mut state, now);
            state
                .pending_pairings
                .values_mut()
                .find(|pending| {
                    pending.mode == DevicePairingModeSnapshot::DirectConfirm
                        && pending.remaining_attempts > 0
                        && pending
                            .key_fingerprint
                            .as_deref()
                            .is_some_and(|fp| fp.eq_ignore_ascii_case(&discovered.key_fingerprint))
                        && pending.peer_address.as_deref().is_some_and(|addr| {
                            addr.parse::<IpAddr>().ok().is_some_and(|ip| {
                                ip.to_canonical() == discovered.peer_ip.to_canonical()
                            })
                        })
                })
                .map(|pending| {
                    pending.remaining_attempts = 0;
                    pending.decision.clone()
                })
        };
        if let Some(decision_tx) = existing_pending {
            self.shared.notify_changed();
            if decision_tx
                .send(PairingDecision::ApproveDirect {
                    display_name: preferred_name.clone(),
                })
                .await
                .is_ok()
            {
                return Ok(());
            }
        }

        let target_expires_at = now
            .checked_add(i64::try_from(TARGETED_PAIRING_TTL.as_millis()).unwrap_or(i64::MAX))
            .ok_or(DeviceGatewayError::Clock)?;
        let (identity, port) = {
            let mut state = self.shared.state.lock().await;
            if !state.enabled || !state.available {
                return Err(DeviceGatewayError::Unavailable);
            }
            let identity = state
                .identity
                .clone()
                .ok_or(DeviceGatewayError::Unavailable)?;
            let port = state.active_port.ok_or(DeviceGatewayError::Unavailable)?;
            let current_window = state.pairing_window_expires_at_ms.unwrap_or(0);
            state.pairing_window_expires_at_ms = Some(current_window.max(target_expires_at));
            state.targeted_auto_approves.insert(
                discovered.key_fingerprint.to_ascii_uppercase(),
                TargetedAutoApprove {
                    peer_ip: discovered.peer_ip,
                    display_name: preferred_name,
                    expires_at_ms: target_expires_at,
                },
            );
            (identity, port)
        };
        self.shared.notify_changed();
        let _ = self.lifecycle.try_send(LifecycleCommand::RefreshDiscovery);

        let local_host = resolve_local_ip_for_peer(discovered.peer_ip).map(|ip| ip.to_string());
        let nudge_url = match discovered.peer_ip {
            IpAddr::V4(ipv4) => format!("http://{ipv4}:{}/ez/connect", discovered.control_port),
            IpAddr::V6(ipv6) => format!("http://[{ipv6}]:{}/ez/connect", discovered.control_port),
        };
        let payload = serde_json::json!({
            "host": local_host.unwrap_or_default(),
            "port": port,
            "path": "/device",
            "installation_id": identity.installation_id,
            "certificate_fingerprint": identity.certificate_fingerprint,
        });
        if let Err(error) = send_connect_nudge(&nudge_url, &payload).await {
            eprintln!(
                "runtime-host: device HTTP connect nudge failed (mDNS fallback stays active): url={nudge_url} error={error}"
            );
        }
        Ok(())
    }

    pub(crate) async fn snapshot(&self) -> Result<DeviceGatewaySnapshot, DeviceGatewayError> {
        let now = now_ms()?;
        let devices = self
            .shared
            .runtime
            .registered_devices()
            .map_err(DeviceGatewayError::Runtime)?;
        let paired_fingerprints = devices
            .iter()
            .filter(|device| device.lifecycle == DeviceLifecycle::Paired)
            .map(|device| super::crypto::public_key_fingerprint(device.public_key.as_bytes()))
            .collect::<std::collections::HashSet<_>>();
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
                        peer_address: connection.peer_address.clone(),
                        client_version: connection.client_version.clone(),
                        mcp_server_keys: connection.mcp_server_keys.clone(),
                        console: connection
                            .console
                            .as_ref()
                            .map(super::console::to_snapshot)
                            .or_else(|| {
                                connection
                                    .hardware
                                    .as_ref()
                                    .map(|_| super::native_console::native_console_snapshot())
                            }),
                        speech_relay: connection.speech_relay,
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
                    mode: pending.mode,
                    peer_address: pending.peer_address.clone(),
                    key_fingerprint: pending.key_fingerprint.clone(),
                    client_version: pending.client_version.clone(),
                },
            )
            .collect::<Vec<_>>();
        pending_pairings
            .sort_by(|left, right| left.pairing_request_id.cmp(&right.pairing_request_id));
        let mut discovered_devices = state
            .discovered_devices
            .values()
            .filter(|record| !paired_fingerprints.contains(&record.key_fingerprint))
            .map(|record| DiscoveredDeviceSnapshot {
                discovery_id: record.discovery_id.clone(),
                display_name: record.display_name.clone(),
                model: record.model.clone(),
                peer_address: record.peer_ip.to_string(),
                control_port: record.control_port,
                key_fingerprint: record.key_fingerprint.clone(),
                firmware_version: record.firmware_version.clone(),
                mac_address: record.mac_address.clone(),
            })
            .collect::<Vec<_>>();
        discovered_devices.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.discovery_id.cmp(&right.discovery_id))
        });
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
            discovered_devices,
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
        let mcp_lock = self.shared.mcp_lock(device_id).await;
        let _guard = mcp_lock.lock().await;
        let _ = self
            .shared
            .runtime
            .remove_device_mcp_servers(device_id)
            .await;
    }

    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<DeviceGatewayEvent> {
        self.shared.events.subscribe()
    }

    pub(crate) fn notify_changed(&self) {
        self.shared.notify_changed();
    }

    /// 当主控会话收到桌面端新输入或取消指令时，立即打断在线终端正在播报的旧长文本 TTS。
    pub(crate) async fn interrupt_controller_playbacks_if_session(
        &self,
        session_id: &assistant_protocol::SessionId,
        reason: &str,
    ) -> usize {
        let Ok(controller) = self.shared.runtime.controller_summary().await else {
            return 0;
        };
        if controller.session_id != *session_id {
            return 0;
        }
        self.shared.interrupt_playbacks(None, reason).await
    }

    pub(crate) async fn console_target(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> Option<DeviceConsoleTarget> {
        self.shared
            .state
            .lock()
            .await
            .connections
            .get(device_id)
            .and_then(|connection| connection.console.clone())
    }

    pub(crate) async fn has_native_console(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> bool {
        self.shared
            .state
            .lock()
            .await
            .connections
            .get(device_id)
            .is_some_and(|connection| connection.console.is_none() && connection.hardware.is_some())
    }

    pub(crate) async fn handle_native_console(
        &self,
        device_id: &assistant_protocol::DeviceId,
        relative_path: &str,
        method: axum::http::Method,
        body: axum::body::Bytes,
        speech: &SpeechServiceHandle,
    ) -> axum::response::Response {
        super::native_console::handle_native_console_request(
            &self.shared,
            device_id,
            relative_path,
            method,
            body,
            speech,
        )
        .await
    }

    #[cfg(test)]
    pub(super) async fn seed_test_identity(&self, identity: Arc<InstallationIdentity>, port: u16) {
        let mut state = self.shared.state.lock().await;
        state.enabled = true;
        state.available = true;
        state.active_port = Some(port);
        state.identity = Some(identity);
    }

    #[cfg(test)]
    pub(super) async fn seed_test_mcp_endpoint(&self, port: u16, token: String) {
        let mut state = self.shared.state.lock().await;
        state.active_mcp_port = Some(port);
        state.active_mcp_token = Some(token);
    }

    #[cfg(test)]
    pub(super) async fn seed_test_discovered_device(
        &self,
        discovery_id: &str,
        display_name: &str,
        peer_ip: IpAddr,
        control_port: u16,
        key_fingerprint: &str,
    ) {
        let mut state = self.shared.state.lock().await;
        state.discovered_devices.insert(
            discovery_id.to_owned(),
            DiscoveredDeviceRecord {
                discovery_id: discovery_id.to_owned(),
                display_name: display_name.to_owned(),
                model: "esp32s31-mosaico".to_owned(),
                peer_ip,
                control_port,
                key_fingerprint: key_fingerprint.to_owned(),
                firmware_version: Some("0.3.0".to_owned()),
                mac_address: Some("30:ed:a0:f4:5b:f0".to_owned()),
            },
        );
    }

    #[cfg(test)]
    pub(super) fn test_shared(&self) -> Arc<GatewayShared> {
        self.shared.clone()
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

    pub(super) async fn mcp_lock(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> Arc<Mutex<()>> {
        self.mcp_locks
            .lock()
            .await
            .entry(device_id.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub(super) async fn register_pending(
        &self,
        registration: PendingPairingRegistration,
    ) -> Result<(i64, u64), DeviceGatewayError> {
        let now = now_ms()?;
        let token = self.next_pending_token.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().await;
        prune_expired(&mut state, now);
        let window_expires = state
            .pairing_window_expires_at_ms
            .filter(|expires_at| *expires_at > now)
            .ok_or(DeviceGatewayError::PairingNotOpen)?;
        let targeted_approval = if registration.mode == DevicePairingModeSnapshot::DirectConfirm
            && let Some(fingerprint) = registration.key_fingerprint.as_deref()
        {
            let normalized_fp = fingerprint.to_ascii_uppercase();
            let matches_peer = state.targeted_auto_approves.get(&normalized_fp).is_some_and(
                |targeted| {
                    targeted.expires_at_ms > now
                        && registration.peer_address.as_deref().is_some_and(|addr| {
                            addr.parse::<IpAddr>().ok().is_some_and(|ip| {
                                ip.to_canonical() == targeted.peer_ip.to_canonical()
                            })
                        })
                },
            );
            if matches_peer {
                state.targeted_auto_approves.remove(&normalized_fp)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(targeted) = targeted_approval {
            let preferred_name = targeted
                .display_name
                .or_else(|| Some(registration.display_name.clone()));
            drop(state);
            let _ = registration.decision.try_send(PairingDecision::ApproveDirect {
                display_name: preferred_name,
            });
            return Ok((window_expires, token));
        }
        let request_id = registration.request_id.clone();
        let reuse_existing = state
            .pending_pairings
            .get(&request_id)
            .is_some_and(|existing| {
                existing.mode == DevicePairingModeSnapshot::Spake2
                    && registration.mode == DevicePairingModeSnapshot::Spake2
            });
        let expires_at_ms = if reuse_existing {
            let pending = state
                .pending_pairings
                .get_mut(&request_id)
                .expect("checked existing entry");
            if pending.remaining_attempts == 0 {
                return Err(DeviceGatewayError::PairingAttemptsExhausted);
            }
            pending.display_name = registration.display_name;
            pending.capabilities = registration.capabilities;
            pending.peer_address = registration.peer_address;
            pending.client_version = registration.client_version;
            pending.token = token;
            pending.decision = registration.decision;
            pending.expires_at_ms
        } else {
            state.pending_pairings.insert(
                request_id,
                PendingPairing {
                    display_name: registration.display_name,
                    capabilities: registration.capabilities,
                    expires_at_ms: window_expires,
                    remaining_attempts: registration.remaining_attempts,
                    mode: registration.mode,
                    peer_address: registration.peer_address,
                    key_fingerprint: registration.key_fingerprint,
                    client_version: registration.client_version,
                    token,
                    decision: registration.decision,
                },
            );
            window_expires
        };
        drop(state);
        self.notify_changed();
        Ok((expires_at_ms, token))
    }

    pub(super) async fn remove_discovered_by_fingerprint(&self, fingerprint: &str) {
        let mut state = self.state.lock().await;
        let before = state.discovered_devices.len();
        state
            .discovered_devices
            .retain(|_, record| !record.key_fingerprint.eq_ignore_ascii_case(fingerprint));
        let changed = state.discovered_devices.len() != before;
        drop(state);
        if changed {
            self.notify_changed();
        }
    }

    pub(super) async fn remove_pending_if_token(&self, request_id: &str, token: u64) {
        let mut state = self.state.lock().await;
        let should_remove = state
            .pending_pairings
            .get(request_id)
            .is_some_and(|pending| pending.token == token);
        if should_remove {
            state.pending_pairings.remove(request_id);
            drop(state);
            self.notify_changed();
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "connection registration records identity, capabilities, metadata, console target, hardware profile, and MCP keys atomically"
    )]
    pub(super) async fn register_connection(
        &self,
        device_id: assistant_protocol::DeviceId,
        connection_id: String,
        capabilities: DeviceCapabilitiesSnapshot,
        output_preference: OutputPreferenceSnapshot,
        peer_address: Option<String>,
        client_version: Option<String>,
        mcp_server_keys: Vec<McpServerKey>,
        console: Option<DeviceConsoleTarget>,
        hardware: Option<DeviceHardwareDeclaration>,
        speech_relay: bool,
    ) -> mpsc::Receiver<ConnectionCommand> {
        let (command_tx, command_rx) = mpsc::channel(CONNECTION_COMMAND_CAPACITY);
        let previous = self.state.lock().await.connections.insert(
            device_id,
            ActiveConnection {
                connection_id,
                connected_at_ms: now_ms().unwrap_or_default(),
                capabilities,
                output_preference,
                peer_address,
                client_version,
                mcp_server_keys,
                console,
                hardware,
                active_speaker_role: 1,
                speech_relay,
                command: command_tx,
            },
        );
        if let Some(previous) = previous {
            let _ = previous.command.try_send(ConnectionCommand::Replaced);
        }
        self.notify_changed();
        command_rx
    }

    pub(super) async fn broadcast_speech_status(&self, status: DeviceSpeechServicesSnapshot) {
        let senders = {
            let state = self.state.lock().await;
            state
                .connections
                .values()
                .map(|connection| connection.command.clone())
                .collect::<Vec<_>>()
        };
        for sender in senders {
            let _ = sender.try_send(ConnectionCommand::SpeechStatusChanged(status));
        }
    }

    /// 向在线终端连接发送打断播报指令，清空当前及排队的旧 TTS 片段（可选排除发起打断的终端自身）。
    pub(super) async fn interrupt_playbacks(
        &self,
        exclude_device: Option<&assistant_protocol::DeviceId>,
        reason: &str,
    ) -> usize {
        let senders = {
            let state = self.state.lock().await;
            state
                .connections
                .iter()
                .filter(|(id, _)| exclude_device.is_none_or(|excluded| excluded != *id))
                .map(|(_, connection)| connection.command.clone())
                .collect::<Vec<_>>()
        };
        let mut interrupted = 0;
        for sender in senders {
            if sender
                .try_send(ConnectionCommand::InterruptPlayback {
                    reason: reason.to_owned(),
                })
                .is_ok()
            {
                interrupted += 1;
            }
        }
        interrupted
    }

    pub(super) async fn update_capabilities(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        capabilities: DeviceCapabilitiesSnapshot,
    ) {
        let changed = {
            let mut state = self.state.lock().await;
            if let Some(connection) = state.connections.get_mut(device_id)
                && connection.connection_id == connection_id
                && connection.capabilities != capabilities
            {
                connection.capabilities = capabilities;
                true
            } else {
                false
            }
        };
        if changed {
            self.notify_changed();
        }
    }

    pub(super) async fn active_mcp_endpoint(&self) -> Option<(u16, String)> {
        let state = self.state.lock().await;
        match (state.active_mcp_port, state.active_mcp_token.as_ref()) {
            (Some(port), Some(token)) => Some((port, token.clone())),
            _ => None,
        }
    }

    pub(super) async fn active_mcp_token(&self) -> Option<String> {
        self.state.lock().await.active_mcp_token.clone()
    }

    pub(super) async fn native_hardware(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> Option<DeviceHardwareDeclaration> {
        self.state
            .lock()
            .await
            .connections
            .get(device_id)
            .and_then(|connection| connection.hardware.clone())
    }

    pub(super) async fn update_hardware_vp_version(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        vp_version: u64,
    ) {
        let mut state = self.state.lock().await;
        if let Some(connection) = state.connections.get_mut(device_id)
            && connection.connection_id == connection_id
            && let Some(hw) = connection.hardware.as_mut()
        {
            hw.vp_version = vp_version;
        }
    }

    pub(super) async fn update_hardware_wake_config(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        config: super::protocol::DeviceWakeConfig,
    ) {
        let mut state = self.state.lock().await;
        if let Some(connection) = state.connections.get_mut(device_id)
            && connection.connection_id == connection_id
            && let Some(hw) = connection.hardware.as_mut()
        {
            hw.wake_config = Some(config);
        }
    }

    pub(super) async fn set_active_speaker_role(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        speaker_role: i32,
    ) {
        let mut state = self.state.lock().await;
        if let Some(connection) = state.connections.get_mut(device_id)
            && connection.connection_id == connection_id
        {
            connection.active_speaker_role = speaker_role.clamp(1, 3);
        }
    }

    pub(super) async fn active_speaker_role(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> i32 {
        self.state
            .lock()
            .await
            .connections
            .get(device_id)
            .map_or(1, |connection| connection.active_speaker_role)
    }

    pub(super) async fn sync_wake_config(
        &self,
        device_id: &assistant_protocol::DeviceId,
        request: super::protocol::WakeConfigSyncRequest,
        timeout_duration: Duration,
    ) -> Result<super::protocol::WakeConfigAck, String> {
        let sender = {
            let state = self.state.lock().await;
            state
                .connections
                .get(device_id)
                .filter(|connection| connection.hardware.is_some())
                .map(|connection| connection.command.clone())
        }
        .ok_or_else(|| "目标直连终端当前离线。".to_owned())?;

        let (tx, rx) = oneshot::channel();
        sender
            .send(ConnectionCommand::SyncWakeConfig {
                request,
                response: Some(tx),
            })
            .await
            .map_err(|_| "向终端发送唤醒配置失败，连接可能已断开。".to_owned())?;

        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(ack)) => Ok(ack),
            Ok(Err(_)) => Err("终端连接已中断，未收到唤醒配置回执。".to_owned()),
            Err(_) => Err("等待终端应用唤醒配置超时。".to_owned()),
        }
    }

    pub(super) async fn broadcast_wake_config_sync(
        &self,
        request: super::protocol::WakeConfigSyncRequest,
    ) -> usize {
        let senders = {
            let state = self.state.lock().await;
            state
                .connections
                .values()
                .filter(|connection| connection.hardware.is_some())
                .map(|connection| connection.command.clone())
                .collect::<Vec<_>>()
        };
        let mut count = 0;
        for sender in senders {
            if sender
                .try_send(ConnectionCommand::SyncWakeConfig {
                    request: request.clone(),
                    response: None,
                })
                .is_ok()
            {
                count += 1;
            }
        }
        count
    }

    pub(super) async fn execute_device_command(
        &self,
        device_id: &assistant_protocol::DeviceId,
        request: DeviceCommandRequest,
        timeout_duration: Duration,
    ) -> Result<DeviceCommandResult, String> {
        let sender = {
            let state = self.state.lock().await;
            state
                .connections
                .get(device_id)
                .filter(|connection| connection.hardware.is_some())
                .map(|connection| connection.command.clone())
        }
        .ok_or_else(|| "目标直连终端当前离线。".to_owned())?;

        let (tx, rx) = oneshot::channel();
        sender
            .send(ConnectionCommand::ExecuteDeviceCommand {
                request,
                response: tx,
            })
            .await
            .map_err(|_| "向终端发送指令失败，连接可能已断开。".to_owned())?;

        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err("终端连接已中断，未收到指令执行回执。".to_owned()),
            Err(_) => Err("等待终端执行指令超时。".to_owned()),
        }
    }

    pub(super) async fn broadcast_voiceprint_sync(&self, request: VoiceprintSyncRequest) -> usize {
        let senders = {
            let state = self.state.lock().await;
            state
                .connections
                .values()
                .filter(|connection| connection.hardware.is_some())
                .map(|connection| connection.command.clone())
                .collect::<Vec<_>>()
        };
        let mut count = 0;
        for sender in senders {
            if sender
                .try_send(ConnectionCommand::SyncVoiceprints(request.clone()))
                .is_ok()
            {
                count += 1;
            }
        }
        count
    }

    pub(super) async fn send_connection_command(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
        command: ConnectionCommand,
    ) {
        let sender = {
            let state = self.state.lock().await;
            state
                .connections
                .get(device_id)
                .filter(|connection| connection.connection_id == connection_id)
                .map(|connection| connection.command.clone())
        };
        if let Some(sender) = sender {
            let _ = sender.send(command).await;
        }
    }

    pub(super) async fn is_current_connection(
        &self,
        device_id: &assistant_protocol::DeviceId,
        connection_id: &str,
    ) -> bool {
        self.state
            .lock()
            .await
            .connections
            .get(device_id)
            .is_some_and(|connection| connection.connection_id == connection_id)
    }

    pub(super) async fn is_device_connected(
        &self,
        device_id: &assistant_protocol::DeviceId,
    ) -> bool {
        self.state.lock().await.connections.contains_key(device_id)
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
    ) -> bool {
        let mut state = self.state.lock().await;
        if state
            .connections
            .get(device_id)
            .is_some_and(|connection| connection.connection_id == connection_id)
        {
            state.connections.remove(device_id);
            drop(state);
            self.notify_changed();
            true
        } else {
            false
        }
    }

    pub(super) fn notify_changed(&self) {
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
    state
        .targeted_auto_approves
        .retain(|_, targeted| targeted.expires_at_ms > now);
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

async fn recv_browser_event(
    rx: &mut Option<mdns_sd::Receiver<mdns_sd::ServiceEvent>>,
) -> Option<mdns_sd::ServiceEvent> {
    let receiver = rx.as_ref()?;
    receiver.recv_async().await.ok()
}

fn is_valid_key_fingerprint(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 9
        && bytes[4] == b'-'
        && bytes[..4].iter().all(|byte| byte.is_ascii_hexdigit())
        && bytes[5..].iter().all(|byte| byte.is_ascii_hexdigit())
}

fn select_preferred_discovered_ip(info: &mdns_sd::ResolvedService) -> Option<IpAddr> {
    let v4_addrs = info.get_addresses_v4();
    let best_ipv4 = v4_addrs
        .iter()
        .copied()
        .find(|ip| !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified())
        .or_else(|| v4_addrs.iter().copied().next());
    match best_ipv4 {
        Some(ip) => Some(IpAddr::V4(ip)),
        None => info
            .get_addresses()
            .iter()
            .map(|scoped| scoped.to_ip_addr().to_canonical())
            .find(|ip| !ip.is_loopback() && !ip.is_unspecified()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "pure field parser tested directly without mocking non_exhaustive ResolvedService"
)]
fn parse_discovered_fields(
    fullname: &str,
    control_port: u16,
    peer_ip: IpAddr,
    paired: Option<&str>,
    pk_fp: Option<&str>,
    name: Option<&str>,
    model: Option<&str>,
    fw: Option<&str>,
    mac: Option<&str>,
) -> Option<DiscoveredDeviceRecord> {
    if control_port == 0 {
        return None;
    }
    if paired.map(str::trim) == Some("1") {
        return None;
    }
    let raw_fp = pk_fp?.trim();
    if !is_valid_key_fingerprint(raw_fp) {
        return None;
    }
    let key_fingerprint = raw_fp.to_ascii_uppercase();

    let fallback_name = fullname
        .strip_suffix(&format!(".{DISCOVERABLE_DEVICE_SERVICE_TYPE}"))
        .unwrap_or(fullname)
        .trim()
        .to_owned();
    let display_name = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if fallback_name.is_empty() {
                "ESP-Mosaico".to_owned()
            } else {
                fallback_name
            }
        });
    let model = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("esp32s31-mosaico")
        .to_owned();
    let firmware_version = fw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mac_address = mac
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    Some(DiscoveredDeviceRecord {
        discovery_id: fullname.to_owned(),
        display_name,
        model,
        peer_ip,
        control_port,
        key_fingerprint,
        firmware_version,
        mac_address,
    })
}

fn parse_discovered_record(info: &mdns_sd::ResolvedService) -> Option<DiscoveredDeviceRecord> {
    let peer_ip = select_preferred_discovered_ip(info)?;
    let props = info.get_properties();
    parse_discovered_fields(
        info.get_fullname(),
        info.get_port(),
        peer_ip,
        props.get_property_val_str("paired"),
        props.get_property_val_str("pk_fp"),
        props.get_property_val_str("name"),
        props.get_property_val_str("model"),
        props.get_property_val_str("fw"),
        props.get_property_val_str("mac"),
    )
}

fn resolve_local_ip_for_peer(peer: IpAddr) -> Option<IpAddr> {
    let bind_addr = match peer {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = std::net::UdpSocket::bind(bind_addr).ok()?;
    socket.connect(SocketAddr::new(peer, 80)).ok()?;
    socket.local_addr().ok().map(|addr| addr.ip().to_canonical())
}

async fn send_connect_nudge(url: &str, payload: &serde_json::Value) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| error.to_string())?;
    let response = client
        .post(url)
        .json(payload)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    Ok(())
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

fn gateway_preference_path(runtime_home: &Path) -> PathBuf {
    runtime_home
        .join(GATEWAY_STATE_DIRECTORY)
        .join(GATEWAY_STATE_FILE)
}

fn load_gateway_enabled_preference(runtime_home: &Path) -> Option<bool> {
    let bytes = fs::read(gateway_preference_path(runtime_home)).ok()?;
    let document: GatewayPreferenceDocument = serde_json::from_slice(&bytes).ok()?;
    (document.schema_version == 1).then_some(document.enabled)
}

fn save_gateway_enabled_preference(runtime_home: &Path, enabled: bool) -> Result<(), String> {
    let path = gateway_preference_path(runtime_home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&GatewayPreferenceDocument {
        schema_version: 1,
        enabled,
    })
    .map_err(|error| error.to_string())?;
    let mut suffix = [0_u8; 6];
    let _ = getrandom::fill(&mut suffix);
    let mut suffix_hex = String::with_capacity(suffix.len() * 2);
    for byte in suffix {
        use std::fmt::Write as _;
        let _ = write!(&mut suffix_hex, "{byte:02x}");
    }
    let tmp_path = path.with_file_name(format!(".{GATEWAY_STATE_FILE}.{suffix_hex}.tmp"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    crate::platform::apply_private_open_options(&mut options);
    let mut file = options.open(&tmp_path).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    let _ = file.sync_all();
    drop(file);
    fs::rename(&tmp_path, &path).map_err(|error| error.to_string())?;
    Ok(())
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
    #[error("device pairing code is invalid")]
    InvalidPairingCode,
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
            Self::InvalidPairingCode => RuntimeErrorInfo::new(
                RuntimeErrorCode::InvalidRequest,
                "device pairing code is invalid",
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

    #[test]
    fn discovered_key_fingerprint_validation_accepts_only_xxxx_xxxx_hex() {
        assert!(is_valid_key_fingerprint("1A2B-3C4D"));
        assert!(is_valid_key_fingerprint("1a2b-3c4d"));
        assert!(!is_valid_key_fingerprint("1A2B3C4D"));
        assert!(!is_valid_key_fingerprint("1A2B-3C4G"));
        assert!(!is_valid_key_fingerprint(""));
    }

    #[test]
    fn parse_discovered_fields_filters_paired_invalid_and_normalizes_fingerprint() {
        let record = parse_discovered_fields(
            "Mosaico-5BF0._ez-device._tcp.local.",
            8765,
            IpAddr::V4(Ipv4Addr::new(192, 168, 110, 88)),
            Some("0"),
            Some("a1b2-c3d4"),
            Some("Mosaico-5BF0"),
            Some("esp32s31-mosaico"),
            Some("0.3.0"),
            Some("30:ed:a0:f4:5b:f0"),
        )
        .expect("valid unpaired device");
        assert_eq!(record.discovery_id, "Mosaico-5BF0._ez-device._tcp.local.");
        assert_eq!(record.display_name, "Mosaico-5BF0");
        assert_eq!(record.peer_ip, IpAddr::V4(Ipv4Addr::new(192, 168, 110, 88)));
        assert_eq!(record.control_port, 8765);
        assert_eq!(record.key_fingerprint, "A1B2-C3D4");
        assert_eq!(record.model, "esp32s31-mosaico");
        assert_eq!(record.firmware_version.as_deref(), Some("0.3.0"));
        assert_eq!(record.mac_address.as_deref(), Some("30:ed:a0:f4:5b:f0"));

        assert!(
            parse_discovered_fields(
                "Mosaico-5BF0._ez-device._tcp.local.",
                8765,
                IpAddr::V4(Ipv4Addr::new(192, 168, 110, 88)),
                Some("1"),
                Some("A1B2-C3D4"),
                None,
                None,
                None,
                None,
            )
            .is_none()
        );
        assert!(
            parse_discovered_fields(
                "Mosaico-5BF0._ez-device._tcp.local.",
                8765,
                IpAddr::V4(Ipv4Addr::new(192, 168, 110, 88)),
                Some("0"),
                Some("invalid"),
                None,
                None,
                None,
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn gateway_enabled_preference_persists_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(load_gateway_enabled_preference(dir.path()), None);
        save_gateway_enabled_preference(dir.path(), true).expect("save true");
        assert_eq!(load_gateway_enabled_preference(dir.path()), Some(true));
        save_gateway_enabled_preference(dir.path(), false).expect("save false");
        assert_eq!(load_gateway_enabled_preference(dir.path()), Some(false));
    }
}
