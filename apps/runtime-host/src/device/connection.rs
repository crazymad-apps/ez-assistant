//! 单条 WSS 连接的配对或已登记设备认证状态机。

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    future::Future,
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    CancelRunRequest, DeviceCapabilitiesSnapshot, DeviceId, DevicePairingModeSnapshot,
    OutputPreferenceSnapshot, SpeechServiceStatusSnapshot, SubmitInputMode, SubmitInputRequest,
};
use assistant_runtime::{
    DeviceInputSource, DeviceLifecycle, DevicePublicKey, InputChannelSource, InputModality,
    NewPairedDevice, OutputPreference, SubmitSessionInputRequest,
};
use axum::{
    extract::{
        ConnectInfo, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    response::Response,
};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::{
    sync::{mpsc, oneshot},
    time::{Instant as TokioInstant, MissedTickBehavior, interval, timeout},
};
use tokio_util::sync::CancellationToken;

use super::{
    crypto::{
        HostPakeState, auth_transcript, decode_base64, encode_base64, pairing_associated_data,
        pairing_bind_transcript, pairing_commit_transcript, public_key_fingerprint,
        random_stream_id, random_token, validate_ed25519_public_key, verify_ed25519,
    },
    gateway::{
        ConnectionCommand, DeviceGatewayError, GatewayShared, MAX_PAIRING_ATTEMPTS,
        PairingDecision, PendingPairingRegistration, PlaybackPreparationResult,
    },
    protocol::{
        ApplicationPing, AuthChallenge, DeviceCommandResult, DeviceHello, DownlinkPcmFrame,
        Envelope, HelloAck, InputAccepted, InputSegmentAccepted, InteractionStateChanged,
        ListenCancel, ListenStart, ListenStop, MessageReplayWindow, OutputPreferenceChanged,
        PCM_FRAME_KIND_RELAY_UPLINK, PCM_FRAME_KIND_VOICE_UPLINK, PairingBind, PairingBindAck,
        PairingCommit, PairingComplete, PairingConfirmation, PairingDirectChallenge,
        PairingDirectCommit, PairingHello, PairingPake, PairingPending, PcmFormat, PlaybackCancel,
        PlaybackEnd, PlaybackStart, ProtocolError, RELAY_DOWNLINK_CHUNK_BYTES,
        RelayDownlinkPcmFrame, RelayUplinkPcmFrame, SetOutputPreference, SpeechAsrCancel,
        SpeechAsrResult, SpeechAsrStart, SpeechAsrStop, SpeechStatusChanged, SpeechTtsEnd,
        SpeechTtsRequest, SpeechTtsStart, TextInput, Transcript, UplinkPcmFrame, VoiceprintSyncAck,
        WakeConfigAck, WireError, WirePairingMode, preference_is_supported,
    },
};
use crate::media_diagnostics::{correlation_id, timestamp_ms};
use crate::speech::SpeechServiceError;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(25);
const PLAYBACK_FRAME_INTERVAL: Duration = Duration::from_millis(20);
const VOICE_TURN_POLL_INTERVAL: Duration = Duration::from_millis(20);
const PLAYBACK_SEND_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_QUEUED_PLAYBACKS: usize = 20;
const MAX_RELAY_REQUEST_ID_BYTES: usize = 128;
const MAX_RELAY_IN_FLIGHT_ASR: usize = 4;
const MAX_RELAY_IN_FLIGHT_TTS: usize = 4;
const MAX_RELAY_TTS_TEXT_CHARS: usize = 120;
const PAIRING_DECISION_QUEUE_CAPACITY: usize = 1;
const MAX_DEVICE_NAME_BYTES: usize = 128;
const MAX_CLIENT_VERSION_BYTES: usize = 128;
const MAX_NONCE_BYTES: usize = 256;
const MAX_CLIENT_INPUT_ID_BYTES: usize = 256;
const MAX_TEXT_INPUT_BYTES: usize = 48 * 1024;
const MAX_UPLINK_PCM_BYTES: usize = 60 * 16_000 * 2;
const MAX_CLIENT_INPUT_FINGERPRINTS: usize = 1_024;
const VOICE_TURN_COMMIT_DELAY: Duration = Duration::from_secs(2);
const MAX_VOICE_TURN_SEGMENTS: usize = 16;
const SPAKE2_P256_SHARE_BYTES: usize = 65;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;
const HMAC_SHA256_BYTES: usize = 32;

pub(super) async fn upgrade(
    State(shared): State<std::sync::Arc<GatewayShared>>,
    ConnectInfo(peer_addr): ConnectInfo<std::net::SocketAddr>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let peer_ip = peer_addr.ip().to_canonical();
    upgrade
        .max_message_size(super::protocol::MAX_CONTROL_MESSAGE_BYTES)
        .max_frame_size(super::protocol::MAX_CONTROL_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(error) = serve(socket, shared, peer_ip).await {
                match error {
                    ConnectionError::Closed => {}
                    other => eprintln!("runtime-host: device connection ended: {other}"),
                }
            }
        })
}

async fn serve(
    mut socket: WebSocket,
    shared: std::sync::Arc<GatewayShared>,
    peer_ip: IpAddr,
) -> Result<(), ConnectionError> {
    let connection_id = random_token(16)?;
    let host_nonce = random_token(32)?;
    send_payload(
        &mut socket,
        "auth_challenge",
        &AuthChallenge {
            connection_id: connection_id.clone(),
            nonce: host_nonce.clone(),
            server_time_ms: now_ms()?,
        },
    )
    .await?;
    let mut replay = MessageReplayWindow::default();
    let first = receive_envelope(&mut socket, &mut replay, HANDSHAKE_TIMEOUT).await?;
    match first.message_type.as_str() {
        "pairing_hello" => {
            let hello = first.payload::<PairingHello>()?;
            pair_device(socket, shared, peer_ip, hello).await
        }
        "hello" => {
            let hello = first.payload::<DeviceHello>()?;
            authenticate_device(
                socket,
                shared,
                peer_ip,
                replay,
                connection_id,
                host_nonce,
                hello,
            )
            .await
        }
        _ => {
            send_wire_error(
                &mut socket,
                "authentication_required",
                Some(first.message_id),
                false,
            )
            .await?;
            close(&mut socket, 1008, "authentication required").await
        }
    }
}

enum ParsedPairingCredential {
    Spake2 { device_share: Vec<u8> },
    DirectConfirm { public_key: Vec<u8> },
}

async fn pair_device(
    mut socket: WebSocket,
    shared: std::sync::Arc<GatewayShared>,
    peer_ip: IpAddr,
    hello: PairingHello,
) -> Result<(), ConnectionError> {
    validate_pairing_hello(&hello)?;
    if !shared.pairing_is_open(now_ms()?).await {
        send_wire_error(&mut socket, "pairing_not_open", None, true).await?;
        return close(&mut socket, 1008, "pairing not open").await;
    }

    let (credential, mode, remaining_attempts, key_fingerprint) = match hello.pairing_mode {
        WirePairingMode::Spake2 => {
            if hello.public_key.is_some() {
                return Err(ConnectionError::InvalidMessage);
            }
            let Some(share) = hello.pake_share.as_deref() else {
                return Err(ConnectionError::InvalidMessage);
            };
            let device_share = decode_base64(share, SPAKE2_P256_SHARE_BYTES)?;
            (
                ParsedPairingCredential::Spake2 { device_share },
                DevicePairingModeSnapshot::Spake2,
                MAX_PAIRING_ATTEMPTS,
                None,
            )
        }
        WirePairingMode::DirectConfirm => {
            if hello.pake_share.is_some() {
                return Err(ConnectionError::InvalidMessage);
            }
            let Some(key_b64) = hello.public_key.as_deref() else {
                return Err(ConnectionError::InvalidMessage);
            };
            let public_key = decode_base64(key_b64, ED25519_PUBLIC_KEY_BYTES)?;
            validate_ed25519_public_key(&public_key)?;
            let fingerprint = public_key_fingerprint(&public_key);
            (
                ParsedPairingCredential::DirectConfirm { public_key },
                DevicePairingModeSnapshot::DirectConfirm,
                1,
                Some(fingerprint),
            )
        }
    };

    let (decision_tx, mut decision_rx) = mpsc::channel(PAIRING_DECISION_QUEUE_CAPACITY);
    let (expires_at_ms, token) = shared
        .register_pending(PendingPairingRegistration {
            request_id: hello.pairing_request_id.clone(),
            display_name: hello.display_name.clone(),
            capabilities: hello.capabilities,
            mode,
            remaining_attempts,
            peer_address: Some(peer_ip.to_string()),
            key_fingerprint,
            client_version: hello.client_version.clone(),
            decision: decision_tx,
        })
        .await?;

    let mut replay = MessageReplayWindow::default();
    let result = run_pairing_wait_and_complete(
        &mut socket,
        &shared,
        &hello,
        credential,
        &mut replay,
        &mut decision_rx,
        expires_at_ms,
    )
    .await;

    // 保留 SPAKE2 输错配对码后的重试计数，其余退出路径（成功、拒绝、断连、免码结束、超时）均立刻清理候选。
    let keep_spake2_retry = matches!(result, Err(ConnectionError::Spake2CodeMismatch));
    if !keep_spake2_retry {
        shared
            .remove_pending_if_token(&hello.pairing_request_id, token)
            .await;
    }
    if keep_spake2_retry { Ok(()) } else { result }
}

async fn run_pairing_wait_and_complete(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    hello: &PairingHello,
    credential: ParsedPairingCredential,
    replay: &mut MessageReplayWindow,
    decision_rx: &mut mpsc::Receiver<PairingDecision>,
    expires_at_ms: i64,
) -> Result<(), ConnectionError> {
    send_payload(
        socket,
        "pairing_pending",
        &PairingPending {
            pairing_request_id: hello.pairing_request_id.clone(),
            expires_at_ms,
        },
    )
    .await?;

    let deadline = TokioInstant::now()
        + Duration::from_millis(
            u64::try_from(expires_at_ms.saturating_sub(now_ms()?)).unwrap_or_default(),
        );
    let decision = wait_for_pairing_decision(socket, decision_rx, replay, deadline).await?;
    match (credential, decision) {
        (_, PairingDecision::Reject) => {
            send_wire_error(socket, "pairing_rejected", None, false).await?;
            close(socket, 1008, "pairing rejected").await
        }
        (
            ParsedPairingCredential::Spake2 { device_share },
            PairingDecision::ApproveSpake2 {
                pairing_code,
                display_name,
            },
        ) => {
            complete_spake2_pairing(
                socket,
                shared,
                hello,
                device_share,
                pairing_code,
                display_name,
            )
            .await
        }
        (
            ParsedPairingCredential::DirectConfirm { public_key },
            PairingDecision::ApproveDirect { display_name },
        ) => {
            complete_direct_pairing(socket, shared, hello, &public_key, display_name, replay).await
        }
        _ => Err(ConnectionError::InvalidMessage),
    }
}

async fn wait_for_pairing_decision(
    socket: &mut WebSocket,
    decision_rx: &mut mpsc::Receiver<PairingDecision>,
    replay: &mut MessageReplayWindow,
    deadline: TokioInstant,
) -> Result<PairingDecision, ConnectionError> {
    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            decision = decision_rx.recv() => {
                return decision.ok_or(ConnectionError::PairingCancelled);
            }
            message = socket.next() => {
                let Some(message) = message else {
                    return Err(ConnectionError::Closed);
                };
                match message? {
                    Message::Close(_) => return Err(ConnectionError::Closed),
                    Message::Ping(payload) => {
                        socket.send(Message::Pong(payload)).await?;
                    }
                    Message::Pong(_) => {}
                    Message::Text(text) => {
                        let envelope = Envelope::decode(&text)?;
                        replay.accept(&envelope.message_id)?;
                        match envelope.message_type.as_str() {
                            "ping" => {
                                let ping = envelope.payload::<ApplicationPing>()?;
                                send_payload(socket, "pong", &ping).await?;
                            }
                            "pong" => {
                                let _ = envelope.payload::<ApplicationPing>()?;
                            }
                            _ => return Err(ConnectionError::InvalidMessage),
                        }
                    }
                    Message::Binary(_) => return Err(ConnectionError::InvalidMessage),
                }
            }
            () = &mut sleep => {
                let _ = send_wire_error(socket, "pairing_expired", None, true).await;
                let _ = close(socket, 1008, "pairing expired").await;
                return Err(ConnectionError::PairingExpired);
            }
        }
    }
}

async fn complete_spake2_pairing(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    hello: &PairingHello,
    device_share: Vec<u8>,
    pairing_code: String,
    decision_display_name: Option<String>,
) -> Result<(), ConnectionError> {
    let identity = shared.installation_identity().await?;
    let host_nonce = random_token(32)?;
    let associated_data = pairing_associated_data(
        &hello.pairing_request_id,
        &identity.installation_id,
        &identity.certificate_fingerprint,
        &hello.device_nonce,
        &host_nonce,
        hello.capabilities,
    );
    let (pake, host_share) = HostPakeState::start(
        &pairing_code,
        &hello.pairing_request_id,
        &identity.installation_id,
        &associated_data,
    )?;
    let keys = pake.finish(&device_share)?;
    send_payload(
        socket,
        "pairing_pake",
        &PairingPake {
            pairing_request_id: hello.pairing_request_id.clone(),
            host_nonce,
            pake_share: encode_base64(&host_share),
            confirmation_mac: encode_base64(keys.host_confirmation()),
        },
    )
    .await?;

    let mut replay = MessageReplayWindow::default();
    let confirmation = receive_typed::<PairingConfirmation>(
        socket,
        &mut replay,
        HANDSHAKE_TIMEOUT,
        "pairing_confirm",
    )
    .await?;
    ensure_pairing_request(&confirmation.pairing_request_id, &hello.pairing_request_id)?;
    let confirmation_mac = decode_base64(&confirmation.confirmation_mac, HMAC_SHA256_BYTES)?;
    if keys.verify_device_confirmation(&confirmation_mac).is_err() {
        send_wire_error(socket, "pairing_failed", None, true).await?;
        let _ = close(socket, 1008, "pairing failed").await;
        return Err(ConnectionError::Spake2CodeMismatch);
    }

    let bind = receive_typed::<PairingBind>(socket, &mut replay, HANDSHAKE_TIMEOUT, "pairing_bind")
        .await?;
    ensure_pairing_request(&bind.pairing_request_id, &hello.pairing_request_id)?;
    let public_key = decode_base64(&bind.public_key, ED25519_PUBLIC_KEY_BYTES)?;
    let signature = decode_base64(&bind.signature, ED25519_SIGNATURE_BYTES)?;
    let binding_mac = decode_base64(&bind.binding_mac, HMAC_SHA256_BYTES)?;
    let bind_transcript = pairing_bind_transcript(&associated_data, &public_key);
    keys.verify_binding_mac(b"device-bind", &bind_transcript, &binding_mac)?;
    verify_ed25519(&public_key, &bind_transcript, &signature)?;

    let device_id = resolve_pairing_device_id(shared, &public_key)?;
    let commit_transcript = pairing_commit_transcript(&bind_transcript, device_id.as_str());
    let host_proof = keys.binding_mac(b"host-bind-ack", &commit_transcript);
    send_payload(
        socket,
        "pairing_bind_ack",
        &PairingBindAck {
            pairing_request_id: hello.pairing_request_id.clone(),
            device_id: device_id.to_string(),
            host_proof: encode_base64(&host_proof),
        },
    )
    .await?;

    let commit =
        receive_typed::<PairingCommit>(socket, &mut replay, HANDSHAKE_TIMEOUT, "pairing_commit")
            .await?;
    ensure_pairing_request(&commit.pairing_request_id, &hello.pairing_request_id)?;
    if commit.device_id != device_id.as_str() {
        return Err(ConnectionError::InvalidMessage);
    }
    let signature = decode_base64(&commit.signature, ED25519_SIGNATURE_BYTES)?;
    let binding_mac = decode_base64(&commit.binding_mac, HMAC_SHA256_BYTES)?;
    keys.verify_binding_mac(b"device-commit", &commit_transcript, &binding_mac)?;
    verify_ed25519(&public_key, &commit_transcript, &signature)?;
    finalize_paired_device(
        socket,
        shared,
        hello,
        device_id,
        &public_key,
        decision_display_name,
    )
    .await
}

async fn complete_direct_pairing(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    hello: &PairingHello,
    public_key: &[u8],
    decision_display_name: Option<String>,
    replay: &mut MessageReplayWindow,
) -> Result<(), ConnectionError> {
    let identity = shared.installation_identity().await?;
    let host_nonce = random_token(32)?;
    let device_id = resolve_pairing_device_id(shared, public_key)?;
    let associated_data = pairing_associated_data(
        &hello.pairing_request_id,
        &identity.installation_id,
        &identity.certificate_fingerprint,
        &hello.device_nonce,
        &host_nonce,
        hello.capabilities,
    );
    let bind_transcript = pairing_bind_transcript(&associated_data, public_key);
    let commit_transcript = pairing_commit_transcript(&bind_transcript, device_id.as_str());

    send_payload(
        socket,
        "pairing_direct_challenge",
        &PairingDirectChallenge {
            pairing_request_id: hello.pairing_request_id.clone(),
            host_nonce,
            device_id: device_id.to_string(),
        },
    )
    .await?;

    let commit = receive_typed::<PairingDirectCommit>(
        socket,
        replay,
        HANDSHAKE_TIMEOUT,
        "pairing_direct_commit",
    )
    .await?;
    ensure_pairing_request(&commit.pairing_request_id, &hello.pairing_request_id)?;
    if commit.device_id != device_id.as_str() {
        return Err(ConnectionError::InvalidMessage);
    }
    let signature = decode_base64(&commit.signature, ED25519_SIGNATURE_BYTES)?;
    if verify_ed25519(public_key, &commit_transcript, &signature).is_err() {
        send_wire_error(socket, "pairing_failed", None, false).await?;
        return close(socket, 1008, "pairing failed").await;
    }

    finalize_paired_device(
        socket,
        shared,
        hello,
        device_id,
        public_key,
        decision_display_name,
    )
    .await
}

fn resolve_pairing_device_id(
    shared: &GatewayShared,
    public_key: &[u8],
) -> Result<DeviceId, ConnectionError> {
    if let Ok(existing_devices) = shared.runtime.registered_devices()
        && let Some(existing) = existing_devices
            .into_iter()
            .find(|device| device.public_key.as_bytes() == public_key)
    {
        return Ok(existing.device_id);
    }
    DeviceId::new(format!("device-{}", random_token(18)?))
        .map_err(|_| ConnectionError::InvalidMessage)
}

async fn finalize_paired_device(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    hello: &PairingHello,
    device_id: DeviceId,
    public_key: &[u8],
    decision_display_name: Option<String>,
) -> Result<(), ConnectionError> {
    let fingerprint = public_key_fingerprint(public_key);
    let public_key =
        DevicePublicKey::from_slice(public_key).ok_or(ConnectionError::InvalidMessage)?;
    let display_name = decision_display_name.unwrap_or_else(|| hello.display_name.clone());
    let stored = match shared
        .runtime
        .register_paired_device(NewPairedDevice {
            device_id,
            display_name,
            public_key,
            paired_at_ms: now_ms()?,
        })
        .await
    {
        Ok(stored) => stored,
        Err(error) => {
            let _ = send_wire_error(socket, "pairing_failed", None, false).await;
            let _ = close(socket, 1008, "pairing failed").await;
            return Err(error.into());
        }
    };
    shared.remove_discovered_by_fingerprint(&fingerprint).await;
    shared.notify_changed();
    send_payload(
        socket,
        "pairing_complete",
        &PairingComplete {
            pairing_request_id: hello.pairing_request_id.clone(),
            device_id: stored.device_id.to_string(),
            display_name: stored.display_name,
        },
    )
    .await?;
    close(socket, 1000, "pairing complete; reconnect to authenticate").await
}

async fn authenticate_device(
    mut socket: WebSocket,
    shared: std::sync::Arc<GatewayShared>,
    peer_ip: IpAddr,
    mut replay: MessageReplayWindow,
    connection_id: String,
    host_nonce: String,
    hello: DeviceHello,
) -> Result<(), ConnectionError> {
    if validate_device_hello(&hello).is_err() {
        return reject_authentication(&mut socket).await;
    }
    let Ok(device_id) = DeviceId::new(hello.device_id.clone()) else {
        return reject_authentication(&mut socket).await;
    };
    let device = match shared.runtime.registered_device(&device_id) {
        Ok(Some(device)) => device,
        Ok(None) | Err(_) => return reject_authentication(&mut socket).await,
    };
    let transcript = auth_transcript(
        &connection_id,
        &host_nonce,
        device_id.as_str(),
        &hello.device_nonce,
        hello.capabilities,
        hello.output_preference,
    );
    let Ok(signature) = decode_base64(&hello.signature, ED25519_SIGNATURE_BYTES) else {
        return reject_authentication(&mut socket).await;
    };
    if verify_ed25519(device.public_key.as_bytes(), &transcript, &signature).is_err() {
        return reject_authentication(&mut socket).await;
    }
    if device.lifecycle == DeviceLifecycle::Revoked {
        return reject_revoked_device(&mut socket).await;
    }
    let capabilities = effective_capabilities(
        hello.capabilities,
        shared.speech.asr_available(),
        shared.speech.tts_available(),
    );
    if !preference_is_supported(capabilities, hello.output_preference) {
        send_wire_error(&mut socket, "unsupported_output_preference", None, true).await?;
        return close(&mut socket, 1008, "unsupported output preference").await;
    }
    let console_target =
        super::console::build_device_console_target(Some(peer_ip), hello.console.as_ref());
    let hardware = hello.hardware.clone();
    let initial_device_vp = hardware.as_ref().map_or(0, |hw| hw.vp_version);
    let hardware_enabled = hardware.is_some();
    let speech_relay = hello.speech_relay;
    send_payload(
        &mut socket,
        "hello_ack",
        &HelloAck {
            device_id: device_id.to_string(),
            connection_id: connection_id.clone(),
            capabilities,
            output_preference: hello.output_preference,
            speech_services: (speech_relay || hardware_enabled)
                .then(|| shared.speech.status()),
        },
    )
    .await?;

    let mut mcp_definitions = super::mcp::build_device_mcp_definitions(
        &device_id,
        &device.display_name,
        peer_ip,
        &hello.mcp_servers,
    );
    if hardware_enabled
        && console_target.is_none()
        && let Some((mcp_port, mcp_token)) = shared.active_mcp_endpoint().await
        && let Some(native_mcp) = super::mcp::build_native_device_mcp_definition(
            &device_id,
            &device.display_name,
            mcp_port,
            &mcp_token,
        )
    {
        mcp_definitions.push(native_mcp);
    }
    let mcp_server_keys = mcp_definitions
        .iter()
        .map(|definition| definition.server_key.clone())
        .collect::<Vec<_>>();
    let mut signals = shared
        .register_connection(
            device_id.clone(),
            connection_id.clone(),
            capabilities,
            hello.output_preference,
            Some(peer_ip.to_string()),
            Some(hello.client_version.clone()),
            mcp_server_keys,
            console_target,
            hardware,
            speech_relay,
        )
        .await;
    if hardware_enabled {
        if let Some(initial_sync) = shared
            .native_console
            .prepare_initial_sync(initial_device_vp)
            .await
        {
            let _ = send_payload(&mut socket, "voiceprint_sync", &initial_sync).await;
        }
        let initial_wake = shared.native_console.build_wake_sync().await;
        let _ = send_payload(&mut socket, "wake_config_sync", &initial_wake).await;
    }
    shared.mcp_tasks.spawn({
        let shared = shared.clone();
        let device_id = device_id.clone();
        let connection_id = connection_id.clone();
        async move {
            let lock = shared.mcp_lock(&device_id).await;
            let _guard = lock.lock().await;
            if shared
                .is_current_connection(&device_id, &connection_id)
                .await
            {
                let _ = shared
                    .runtime
                    .sync_device_mcp_servers(&device_id, mcp_definitions)
                    .await;
            }
        }
    });

    let result = authenticated_loop(
        &mut socket,
        &shared,
        AuthenticatedLoopConfig {
            device_id: &device_id,
            connection_id: &connection_id,
            declared_capabilities: hello.capabilities,
            capabilities,
            speech_relay,
            hardware_enabled,
        },
        &mut replay,
        &mut signals,
    )
    .await;
    if shared
        .unregister_connection(&device_id, &connection_id)
        .await
    {
        let lock = shared.mcp_lock(&device_id).await;
        let _guard = lock.lock().await;
        if !shared.is_device_connected(&device_id).await {
            let _ = shared.runtime.mark_device_mcp_offline(&device_id).await;
        }
    }
    result
}

struct AuthenticatedLoopConfig<'a> {
    device_id: &'a DeviceId,
    connection_id: &'a str,
    declared_capabilities: DeviceCapabilitiesSnapshot,
    capabilities: DeviceCapabilitiesSnapshot,
    speech_relay: bool,
    hardware_enabled: bool,
}

async fn authenticated_loop(
    socket: &mut WebSocket,
    shared: &Arc<GatewayShared>,
    config: AuthenticatedLoopConfig<'_>,
    replay: &mut MessageReplayWindow,
    commands: &mut mpsc::Receiver<ConnectionCommand>,
) -> Result<(), ConnectionError> {
    let AuthenticatedLoopConfig {
        device_id,
        connection_id,
        declared_capabilities,
        mut capabilities,
        speech_relay,
        hardware_enabled,
    } = config;
    let mut heartbeat = interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await;
    let mut playback_clock = interval(PLAYBACK_FRAME_INTERVAL);
    playback_clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
    playback_clock.tick().await;
    let mut voice_turn_clock = interval(VOICE_TURN_POLL_INTERVAL);
    voice_turn_clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
    voice_turn_clock.tick().await;
    let mut last_received = Instant::now();
    let mut receiving: Option<UplinkUtterance> = None;
    let voice_turn = shared.voice_turn(device_id).await;
    {
        let mut aggregation = voice_turn.lock().await;
        if aggregation
            .as_ref()
            .is_some_and(VoiceTurnAggregation::is_empty)
        {
            *aggregation = None;
        } else if let Some(aggregation) = aggregation.as_mut() {
            // Give a reconnected terminal time to retransmit a segment whose
            // ownership acknowledgement may have been lost with the old WSS.
            aggregation.schedule_commit();
        }
    }
    let mut playback = VecDeque::<ActivePlayback>::new();
    let mut client_inputs = ClientInputWindow::default();
    let mut relay = RelayState::new();
    let mut pending_commands: HashMap<String, oneshot::Sender<DeviceCommandResult>> =
        HashMap::new();
    let mut pending_wake_syncs: HashMap<String, oneshot::Sender<WakeConfigAck>> = HashMap::new();
    loop {
        tokio::select! {
            _ = voice_turn_clock.tick() => {
                process_voice_turn(socket, shared, device_id, capabilities, &voice_turn).await?;
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    return Err(ConnectionError::Closed);
                };
                match command {
                    ConnectionCommand::Replaced => {
                        cancel_playback(socket, &mut playback, "replaced").await?;
                        send_wire_error(socket, "connection_replaced", None, false).await?;
                        return close(socket, 1008, "connection_replaced").await;
                    }
                    ConnectionCommand::Revoked => {
                        cancel_playback(socket, &mut playback, "revoked").await?;
                        send_wire_error(socket, "device_revoked", None, false).await?;
                        return close(socket, 1008, "device_revoked").await;
                    }
                    ConnectionCommand::GatewayDisabled => {
                        cancel_playback(socket, &mut playback, "shutdown").await?;
                        send_wire_error(socket, "gateway_disabled", None, false).await?;
                        return close(socket, 1008, "gateway_disabled").await;
                    }
                    ConnectionCommand::TextOutput(output) => {
                        send_payload(socket, "text_output", &output).await?;
                    }
                    ConnectionCommand::OutputUnavailable(state) => {
                        send_payload(socket, "state_changed", &state).await?;
                    }
                    ConnectionCommand::PreparePlayback(preparation) => {
                        let result = if preparation.response.is_closed() || preparation.cancellation.is_cancelled() || receiving.is_some() || voice_turn.lock().await.is_some() {
                            preparation.cancellation.cancel();
                            PlaybackPreparationResult::Interrupted
                        } else if reserve_playback(
                            &mut playback,
                            preparation.output_id,
                            preparation.cancellation.clone(),
                        ) {
                            PlaybackPreparationResult::Accepted
                        } else {
                            preparation.cancellation.cancel();
                            PlaybackPreparationResult::CapacityExceeded
                        };
                        if preparation.response.send(result).is_err() {
                            preparation.cancellation.cancel();
                        }
                    }
                    ConnectionCommand::StartPlayback { output, response } => {
                        acknowledge_playback_output(&mut playback, output, response);
                    }
                    ConnectionCommand::SpeechStatusChanged(speech_services) => {
                        let asr_ready =
                            speech_services.asr != SpeechServiceStatusSnapshot::Unavailable;
                        let tts_ready =
                            speech_services.tts != SpeechServiceStatusSnapshot::Unavailable;
                        let next_caps =
                            effective_capabilities(declared_capabilities, asr_ready, tts_ready);
                        if next_caps != capabilities {
                            capabilities = next_caps;
                            shared
                                .update_capabilities(device_id, connection_id, capabilities)
                                .await;
                        }
                        if speech_relay || hardware_enabled {
                            send_payload(
                                socket,
                                "speech_status_changed",
                                &SpeechStatusChanged { speech_services },
                            )
                            .await?;
                        }
                    }
                    ConnectionCommand::SpeechAsrResult(result) => {
                        if relay.in_flight_asr.remove(&result.request_id).is_some() {
                            send_payload(socket, "speech_asr_result", &result).await?;
                        }
                    }
                    ConnectionCommand::SpeechTtsResult {
                        request_id,
                        stream_id,
                        result,
                    } => {
                        deliver_relay_tts_result(socket, &mut relay, request_id, stream_id, result)
                            .await?;
                    }
                    ConnectionCommand::ExecuteDeviceCommand { request, response } => {
                        if pending_commands.len() >= 32 {
                            let _ = response.send(DeviceCommandResult {
                                command_id: request.command_id,
                                ok: false,
                                result_json: String::new(),
                                media_url: String::new(),
                                error_code: "DEVICE_BUSY".to_owned(),
                                error_message: "终端并发指令队列已满".to_owned(),
                            });
                        } else {
                            pending_commands.insert(request.command_id.clone(), response);
                            send_payload(socket, "device_command", &request).await?;
                        }
                    }
                    ConnectionCommand::SyncVoiceprints(sync) => {
                        if hardware_enabled {
                            send_payload(socket, "voiceprint_sync", &sync).await?;
                        }
                    }
                    ConnectionCommand::SyncWakeConfig { request, response } => {
                        if hardware_enabled {
                            if let Some(tx) = response {
                                if pending_wake_syncs.len() >= 16 {
                                    let _ = tx.send(WakeConfigAck {
                                        sync_id: request.sync_id,
                                        ok: false,
                                        active_config: None,
                                        error: Some("终端并发唤醒配置更新队列已满".to_owned()),
                                    });
                                    continue;
                                }
                                pending_wake_syncs.insert(request.sync_id.clone(), tx);
                            }
                            send_payload(socket, "wake_config_sync", &request).await?;
                        } else if let Some(tx) = response {
                            let _ = tx.send(WakeConfigAck {
                                sync_id: request.sync_id,
                                ok: false,
                                active_config: None,
                                error: Some("终端不支持直连唤醒配置".to_owned()),
                            });
                        }
                    }
                }
                synchronize_playback(socket, &mut playback).await?;
            }
            _ = playback_clock.tick() => {
                advance_playback(socket, &mut playback).await?;
            }
            _ = heartbeat.tick() => {
                if last_received.elapsed() > HEARTBEAT_TIMEOUT {
                    return close(socket, 1001, "heartbeat timeout").await;
                }
                send_payload(socket, "ping", &ApplicationPing {
                    nonce: random_token(12)?,
                    sent_at_ms: now_ms()?,
                }).await?;
            }
            message = socket.next() => {
                let Some(message) = message else {
                    return Err(ConnectionError::Closed);
                };
                match message? {
                    Message::Text(text) => {
                        last_received = Instant::now();
                        let envelope = Envelope::decode(&text)?;
                        replay.accept(&envelope.message_id)?;
                        match envelope.message_type.as_str() {
                            "ping" => {
                                let ping = envelope.payload::<ApplicationPing>()?;
                                send_payload(socket, "pong", &ping).await?;
                            }
                            "pong" => {
                                let _ = envelope.payload::<ApplicationPing>()?;
                            }
                            "set_output_preference" => {
                                let request = envelope.payload::<SetOutputPreference>()?;
                                if !preference_is_supported(capabilities, request.output_preference) {
                                    send_wire_error(
                                        socket,
                                        "unsupported_output_preference",
                                        Some(envelope.message_id),
                                        true,
                                    ).await?;
                                    continue;
                                }
                                shared.update_preference(
                                    device_id,
                                    connection_id,
                                    request.output_preference,
                                ).await;
                                send_payload(socket, "output_preference_changed", &OutputPreferenceChanged {
                                    output_preference: request.output_preference,
                                }).await?;
                            }
                            "text_input" => {
                                let request = envelope.payload::<TextInput>()?;
                                if !capabilities.input_text {
                                    send_wire_error(
                                        socket,
                                        "unsupported_input_capability",
                                        Some(envelope.message_id),
                                        true,
                                    ).await?;
                                    continue;
                                }
                                if validate_text_input(&request).is_err()
                                    || !preference_is_supported(capabilities, request.output_preference)
                                {
                                    send_wire_error(
                                        socket,
                                        "invalid_text_input",
                                        Some(envelope.message_id),
                                        true,
                                    ).await?;
                                    continue;
                                }
                                cancel_playback(socket, &mut playback, "interrupted_by_input").await?;
                                if !client_inputs.remember(
                                    &request.client_input_id,
                                    input_fingerprint(b"text", request.text.as_bytes(), request.output_preference),
                                ) {
                                    send_wire_error(socket, "client_input_reused", Some(envelope.message_id), false).await?;
                                    continue;
                                }
                                match submit_text_input(shared, device_id, request).await {
                                    Ok(accepted) => {
                                        send_payload(socket, "input_accepted", &accepted).await?;
                                    }
                                    Err(error) => {
                                        let (code, recoverable) = input_error(&error);
                                        send_wire_error(
                                            socket,
                                            code,
                                            Some(envelope.message_id),
                                            recoverable,
                                        ).await?;
                                    }
                                }
                            }
                            "listen_start" => {
                                let request = envelope.payload::<ListenStart>()?;
                                if !capabilities.input_pcm16_16k_mono || !shared.speech.asr_available() {
                                    send_wire_error(socket, "asr_unavailable", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                if receiving.is_some() {
                                    send_wire_error(socket, "audio_input_busy", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                if validate_listen_start(&request).is_err()
                                    || !preference_is_supported(capabilities, request.output_preference)
                                {
                                    send_wire_error(socket, "invalid_listen_start", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                if voice_turn.lock().await.as_ref().is_some_and(|aggregation| {
                                    !aggregation.can_start_segment(
                                        &request.client_input_id,
                                        request.output_preference,
                                    )
                                }) {
                                    send_wire_error(socket, "voice_turn_limit_reached", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                cancel_playback(socket, &mut playback, "interrupted_by_input").await?;
                                if voice_turn.lock().await.is_none() {
                                    if let Err(error) = cancel_active_controller_run(shared.runtime.as_ref()).await {
                                        let (code, recoverable) = input_error(&error);
                                        send_wire_error(socket, code, Some(envelope.message_id.clone()), recoverable).await?;
                                        continue;
                                    }
                                    *voice_turn.lock().await = Some(VoiceTurnAggregation::new(
                                        request.client_input_id.clone(),
                                        request.output_preference,
                                    ));
                                }
                                if let Some(aggregation) = voice_turn.lock().await.as_mut() {
                                    aggregation.pause_commit();
                                }
                                let client_input_id = request.client_input_id.clone();
                                if let Some(role) = request.speaker_role {
                                    shared
                                        .set_active_speaker_role(device_id, connection_id, role)
                                        .await;
                                }
                                eprintln!(
                                    "event=voice_capture_started ts_ms={} device={} input={} stream_id={} wake_source={} speaker={}",
                                    timestamp_ms(),
                                    correlation_id(device_id.as_str()),
                                    correlation_id(&client_input_id),
                                    request.stream_id,
                                    request.wake_source.as_deref().unwrap_or("unspecified"),
                                    request.speaker_id.as_deref().unwrap_or("anonymous"),
                                );
                                receiving = Some(UplinkUtterance::new(request));
                                send_payload(socket, "state_changed", &InteractionStateChanged {
                                    run_id: None,
                                    client_input_id: Some(client_input_id),
                                    state: "listening".to_owned(),
                                    reason: None,
                                }).await?;
                            }
                            "listen_stop" => {
                                let request = envelope.payload::<ListenStop>()?;
                                let Some(utterance) = receiving.as_ref() else {
                                    send_wire_error(socket, "audio_stream_not_active", Some(envelope.message_id), true).await?;
                                    continue;
                                };
                                if utterance.stream_id != request.stream_id {
                                    send_wire_error(socket, "invalid_listen_stop", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                if utterance.pcm.is_empty() {
                                    let Some(empty) = receiving.take() else {
                                        return Err(ConnectionError::InvalidMessage);
                                    };
                                    let mut aggregation = voice_turn.lock().await;
                                    resume_or_clear_voice_turn(&mut aggregation);
                                    drop(aggregation);
                                    send_payload(socket, "state_changed", &InteractionStateChanged {
                                        run_id: None,
                                        client_input_id: Some(empty.client_input_id),
                                        state: "idle".to_owned(),
                                        reason: Some("no_speech_recognized".to_owned()),
                                    }).await?;
                                    continue;
                                }
                                if utterance.last_sequence() != Some(request.last_sequence) {
                                    send_wire_error(socket, "invalid_listen_stop", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                // 完整校验后才转移 PCM 所有权；可恢复的错误 stop 不能破坏活动采集。
                                let Some(utterance) = receiving.take() else {
                                    return Err(ConnectionError::InvalidMessage);
                                };
                                if !client_inputs.remember(
                                    &utterance.client_input_id,
                                    input_fingerprint(b"speech", &utterance.pcm, utterance.output_preference),
                                ) {
                                    send_wire_error(socket, "client_input_reused", Some(envelope.message_id), false).await?;
                                    continue;
                                }
                                let client_input_id = utterance.client_input_id.clone();
                                let stream_id = utterance.stream_id;
                                let fingerprint = input_fingerprint(
                                    b"speech",
                                    &utterance.pcm,
                                    utterance.output_preference,
                                );
                                let mut aggregation_guard = voice_turn.lock().await;
                                let Some(aggregation) = aggregation_guard.as_mut() else {
                                    send_wire_error(socket, "audio_stream_not_active", Some(envelope.message_id), true).await?;
                                    continue;
                                };
                                let admission = aggregation.admit_segment(
                                    client_input_id.clone(),
                                    fingerprint,
                                    stream_id,
                                );
                                let Some(admission) = admission else {
                                    send_wire_error(socket, "voice_turn_limit_reached", Some(envelope.message_id), true).await?;
                                    continue;
                                };
                                aggregation.schedule_commit();
                                let logical_client_input_id =
                                    aggregation.logical_client_input_id.clone();
                                let recognition = match admission {
                                    SegmentAdmission::New { ordinal, cancellation } => {
                                        Some((ordinal, cancellation, logical_client_input_id))
                                    }
                                    SegmentAdmission::Duplicate => None,
                                };
                                drop(aggregation_guard);
                                if let Some((ordinal, cancellation, logical_client_input_id)) = recognition {
                                    spawn_segment_recognition(
                                        shared,
                                        device_id.clone(),
                                        utterance,
                                        ordinal,
                                        cancellation,
                                        voice_turn.clone(),
                                        logical_client_input_id,
                                    );
                                }
                                send_payload(socket, "input_segment_accepted", &InputSegmentAccepted {
                                    client_input_id: client_input_id.clone(),
                                    stream_id,
                                }).await?;
                                send_payload(socket, "state_changed", &InteractionStateChanged {
                                    run_id: None,
                                    client_input_id: Some(client_input_id),
                                    state: "recognizing".to_owned(),
                                    reason: None,
                                }).await?;
                            }
                            "listen_cancel" => {
                                let request = envelope.payload::<ListenCancel>()?;
                                if receiving.as_ref().is_some_and(|active| active.stream_id == request.stream_id) {
                                    if let Some(cancelled) = receiving.take() {
                                        let mut aggregation = voice_turn.lock().await;
                                        resume_or_clear_voice_turn(&mut aggregation);
                                        drop(aggregation);
                                        send_payload(socket, "state_changed", &InteractionStateChanged {
                                            run_id: None,
                                            client_input_id: Some(cancelled.client_input_id),
                                            state: "idle".to_owned(),
                                            reason: Some("cancelled".to_owned()),
                                        }).await?;
                                    }
                                } else if let Some(cancellation) = voice_turn
                                    .lock()
                                    .await
                                    .as_ref()
                                    .and_then(|aggregation| aggregation.cancellation(request.stream_id))
                                {
                                    cancellation.cancel();
                                } else {
                                    send_wire_error(socket, "audio_stream_not_active", Some(envelope.message_id), true).await?;
                                }
                            }
                            "playback_cancel" => {
                                let request = envelope.payload::<PlaybackCancel>()?;
                                if !capabilities.playback_cancel {
                                    send_wire_error(socket, "unsupported_playback_cancel", Some(envelope.message_id), true).await?;
                                    continue;
                                }
                                let matches = playback.front().is_some_and(|active| {
                                    active.output_id == request.output_id
                                        && active.stream_id() == Some(request.stream_id)
                                });
                                if matches {
                                    cancel_playback(socket, &mut playback, "cancelled").await?;
                                } else {
                                    send_wire_error(socket, "playback_not_active", Some(envelope.message_id), true).await?;
                                }
                            }
                            "speech_asr_start" if speech_relay => {
                                let request = envelope.payload::<SpeechAsrStart>()?;
                                handle_relay_asr_start(
                                    socket,
                                    shared,
                                    &mut relay,
                                    envelope.message_id,
                                    request,
                                ).await?;
                            }
                            "speech_asr_stop" if speech_relay => {
                                let request = envelope.payload::<SpeechAsrStop>()?;
                                handle_relay_asr_stop(
                                    socket,
                                    shared,
                                    device_id,
                                    connection_id,
                                    &mut relay,
                                    request,
                                ).await?;
                            }
                            "speech_asr_cancel" if speech_relay => {
                                let request = envelope.payload::<SpeechAsrCancel>()?;
                                handle_relay_asr_cancel(
                                    socket,
                                    &mut relay,
                                    envelope.message_id,
                                    request,
                                ).await?;
                            }
                            "speech_tts_request" if speech_relay => {
                                let request = envelope.payload::<SpeechTtsRequest>()?;
                                handle_relay_tts_request(
                                    socket,
                                    shared,
                                    device_id,
                                    connection_id,
                                    &mut relay,
                                    request,
                                ).await?;
                            }
                            "device_command_result" if hardware_enabled => {
                                let result = envelope.payload::<DeviceCommandResult>()?;
                                if let Some(tx) = pending_commands.remove(&result.command_id) {
                                    let _ = tx.send(result);
                                }
                            }
                            "voiceprint_sync_ack" if hardware_enabled => {
                                let ack = envelope.payload::<VoiceprintSyncAck>()?;
                                if ack.ok {
                                    shared
                                        .update_hardware_vp_version(
                                            device_id,
                                            connection_id,
                                            ack.vp_version,
                                        )
                                        .await;
                                }
                            }
                            "wake_config_ack" if hardware_enabled => {
                                let ack = envelope.payload::<WakeConfigAck>()?;
                                if ack.ok
                                    && let Some(cfg) = ack.active_config.clone()
                                {
                                    shared
                                        .update_hardware_wake_config(device_id, connection_id, cfg)
                                        .await;
                                }
                                if let Some(tx) = pending_wake_syncs.remove(&ack.sync_id) {
                                    let _ = tx.send(ack);
                                }
                            }
                            _ => {
                                send_wire_error(
                                    socket,
                                    "unsupported_message",
                                    Some(envelope.message_id),
                                    true,
                                ).await?;
                            }
                        }
                    }
                    Message::Ping(value) => {
                        last_received = Instant::now();
                        socket.send(Message::Pong(value)).await?;
                    }
                    Message::Pong(_) => last_received = Instant::now(),
                    Message::Close(_) => return Err(ConnectionError::Closed),
                    Message::Binary(bytes) => {
                        last_received = Instant::now();
                        match bytes.get(1).copied() {
                            Some(PCM_FRAME_KIND_VOICE_UPLINK) => {
                                let frame = match UplinkPcmFrame::decode(&bytes) {
                                    Ok(frame) => frame,
                                    Err(_) => {
                                        send_wire_error(socket, "invalid_pcm_frame", None, false).await?;
                                        return close(socket, 1008, "invalid PCM frame").await;
                                    }
                                };
                                let Some(utterance) = receiving.as_mut() else {
                                    send_wire_error(socket, "pcm_without_active_stream", None, true).await?;
                                    continue;
                                };
                                if let Err(error) = utterance.push(frame) {
                                    let code = match error {
                                        UplinkError::WrongStream => "pcm_stream_mismatch",
                                        UplinkError::Sequence => "invalid_pcm_sequence",
                                        UplinkError::TooLarge => "audio_input_too_large",
                                    };
                                    send_wire_error(socket, code, None, false).await?;
                                    return close(socket, 1008, "invalid PCM stream").await;
                                }
                            }
                            Some(PCM_FRAME_KIND_RELAY_UPLINK) if speech_relay => {
                                let frame = match RelayUplinkPcmFrame::decode(&bytes) {
                                    Ok(frame) => frame,
                                    Err(_) => {
                                        send_wire_error(socket, "invalid_pcm_frame", None, false).await?;
                                        return close(socket, 1008, "invalid relay PCM frame").await;
                                    }
                                };
                                push_relay_asr_frame(socket, &mut relay, frame).await?;
                            }
                            _ => {
                                send_wire_error(socket, "invalid_pcm_frame", None, false).await?;
                                return close(socket, 1008, "invalid PCM frame").await;
                            }
                        }
                    }
                }
            }
        }
    }
}

struct RelayConnectionGuard(CancellationToken);

impl Drop for RelayConnectionGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct RelayAsrUpload {
    request_id: String,
    stream_id: u32,
    next_sequence: u32,
    pcm: Vec<u8>,
}

struct RelayState {
    active_asr: Option<RelayAsrUpload>,
    in_flight_asr: HashMap<String, CancellationToken>,
    in_flight_tts: HashSet<String>,
    connection_cancel: CancellationToken,
    _guard: RelayConnectionGuard,
}

impl RelayState {
    fn new() -> Self {
        let connection_cancel = CancellationToken::new();
        Self {
            active_asr: None,
            in_flight_asr: HashMap::new(),
            in_flight_tts: HashSet::new(),
            connection_cancel: connection_cancel.clone(),
            _guard: RelayConnectionGuard(connection_cancel),
        }
    }
}

async fn send_speech_asr_error(
    socket: &mut WebSocket,
    request_id: String,
    code: &str,
) -> Result<(), ConnectionError> {
    send_payload(
        socket,
        "speech_asr_result",
        &SpeechAsrResult {
            request_id,
            ok: false,
            text: None,
            error: Some(code.to_owned()),
        },
    )
    .await
}

async fn send_speech_tts_error(
    socket: &mut WebSocket,
    request_id: String,
    stream_id: u32,
    code: &str,
) -> Result<(), ConnectionError> {
    send_payload(
        socket,
        "speech_tts_start",
        &SpeechTtsStart {
            request_id,
            stream_id,
            ok: false,
            format: None,
            total_bytes: None,
            error: Some(code.to_owned()),
        },
    )
    .await
}

async fn handle_relay_asr_start(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    relay: &mut RelayState,
    message_id: String,
    request: SpeechAsrStart,
) -> Result<(), ConnectionError> {
    if request.request_id.trim().is_empty()
        || request.request_id.len() > MAX_RELAY_REQUEST_ID_BYTES
        || request.stream_id == 0
        || !request.format.is_relay_pcm()
    {
        return send_wire_error(socket, "invalid_speech_asr_request", Some(message_id), true).await;
    }
    if !shared.speech.asr_available() {
        return send_speech_asr_error(socket, request.request_id, "asr_unavailable").await;
    }
    if relay.active_asr.is_some() || relay.in_flight_asr.contains_key(&request.request_id) {
        return send_speech_asr_error(socket, request.request_id, "audio_input_busy").await;
    }
    if relay.in_flight_asr.len() >= MAX_RELAY_IN_FLIGHT_ASR {
        return send_speech_asr_error(socket, request.request_id, "busy").await;
    }
    relay.active_asr = Some(RelayAsrUpload {
        request_id: request.request_id,
        stream_id: request.stream_id,
        next_sequence: 0,
        pcm: Vec::new(),
    });
    Ok(())
}

async fn push_relay_asr_frame(
    socket: &mut WebSocket,
    relay: &mut RelayState,
    frame: RelayUplinkPcmFrame<'_>,
) -> Result<(), ConnectionError> {
    let Some(upload) = relay.active_asr.as_mut() else {
        return send_wire_error(socket, "pcm_without_active_stream", None, true).await;
    };
    if upload.stream_id != frame.stream_id {
        let request_id = upload.request_id.clone();
        relay.active_asr = None;
        return send_speech_asr_error(socket, request_id, "pcm_stream_mismatch").await;
    }
    if upload.next_sequence != frame.sequence {
        let request_id = upload.request_id.clone();
        relay.active_asr = None;
        return send_speech_asr_error(socket, request_id, "invalid_pcm_sequence").await;
    }
    if upload.pcm.len().saturating_add(frame.payload.len()) > MAX_UPLINK_PCM_BYTES {
        let request_id = upload.request_id.clone();
        relay.active_asr = None;
        return send_speech_asr_error(socket, request_id, "audio_input_too_large").await;
    }
    upload.next_sequence = upload.next_sequence.saturating_add(1);
    upload.pcm.extend_from_slice(frame.payload);
    Ok(())
}

async fn handle_relay_asr_stop(
    socket: &mut WebSocket,
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    connection_id: &str,
    relay: &mut RelayState,
    request: SpeechAsrStop,
) -> Result<(), ConnectionError> {
    let Some(active) = relay.active_asr.as_ref() else {
        return send_speech_asr_error(socket, request.request_id, "audio_stream_not_active").await;
    };
    if active.request_id != request.request_id || active.stream_id != request.stream_id {
        return send_speech_asr_error(socket, request.request_id, "audio_stream_not_active").await;
    }
    let Some(upload) = relay.active_asr.take() else {
        return Ok(());
    };
    if upload.pcm.is_empty() || upload.next_sequence != request.last_sequence.saturating_add(1) {
        return send_speech_asr_error(socket, upload.request_id, "invalid_speech_asr_stop").await;
    }
    if relay.in_flight_asr.len() >= MAX_RELAY_IN_FLIGHT_ASR {
        return send_speech_asr_error(socket, upload.request_id, "busy").await;
    }
    let cancel = relay.connection_cancel.child_token();
    relay
        .in_flight_asr
        .insert(upload.request_id.clone(), cancel.clone());
    shared.recognition_tasks.spawn({
        let shared = shared.clone();
        let device_id = device_id.clone();
        let connection_id = connection_id.to_owned();
        let request_id = upload.request_id;
        let pcm = upload.pcm;
        async move {
            let debug_name = format!("relay-{device_id}-{request_id}");
            let result = tokio::spawn({
                let speech = shared.speech.clone();
                async move { speech.recognize(pcm, debug_name, cancel).await }
            })
            .await;
            let payload = match result {
                Ok(Ok(text)) => SpeechAsrResult {
                    request_id,
                    ok: true,
                    text: Some(text),
                    error: None,
                },
                Ok(Err(error)) => SpeechAsrResult {
                    request_id,
                    ok: false,
                    text: None,
                    error: Some(error.code().to_owned()),
                },
                Err(_) => SpeechAsrResult {
                    request_id,
                    ok: false,
                    text: None,
                    error: Some("provider_failed".to_owned()),
                },
            };
            shared
                .send_connection_command(
                    &device_id,
                    &connection_id,
                    ConnectionCommand::SpeechAsrResult(payload),
                )
                .await;
        }
    });
    Ok(())
}

async fn handle_relay_asr_cancel(
    socket: &mut WebSocket,
    relay: &mut RelayState,
    message_id: String,
    request: SpeechAsrCancel,
) -> Result<(), ConnectionError> {
    if relay
        .active_asr
        .as_ref()
        .is_some_and(|active| active.request_id == request.request_id)
    {
        relay.active_asr = None;
        return send_speech_asr_error(socket, request.request_id, "cancelled").await;
    }
    if let Some(cancel) = relay.in_flight_asr.remove(&request.request_id) {
        cancel.cancel();
        return send_speech_asr_error(socket, request.request_id, "cancelled").await;
    }
    send_wire_error(socket, "audio_stream_not_active", Some(message_id), true).await
}

async fn handle_relay_tts_request(
    socket: &mut WebSocket,
    shared: &Arc<GatewayShared>,
    device_id: &DeviceId,
    connection_id: &str,
    relay: &mut RelayState,
    request: SpeechTtsRequest,
) -> Result<(), ConnectionError> {
    if request.request_id.trim().is_empty()
        || request.request_id.len() > MAX_RELAY_REQUEST_ID_BYTES
        || request.stream_id == 0
        || request.text.trim().is_empty()
        || request.text.chars().count() > MAX_RELAY_TTS_TEXT_CHARS
    {
        return send_speech_tts_error(
            socket,
            request.request_id,
            request.stream_id,
            "invalid_input",
        )
        .await;
    }
    if !shared.speech.tts_available() {
        return send_speech_tts_error(
            socket,
            request.request_id,
            request.stream_id,
            "tts_unavailable",
        )
        .await;
    }
    if relay.in_flight_tts.len() >= MAX_RELAY_IN_FLIGHT_TTS
        || !relay.in_flight_tts.insert(request.request_id.clone())
    {
        return send_speech_tts_error(socket, request.request_id, request.stream_id, "busy").await;
    }
    let cancel = relay.connection_cancel.child_token();
    shared.recognition_tasks.spawn({
        let shared = shared.clone();
        let device_id = device_id.clone();
        let connection_id = connection_id.to_owned();
        let request_id = request.request_id;
        let stream_id = request.stream_id;
        let text = request.text;
        async move {
            let debug_name = format!("relay-tts-{device_id}-{request_id}");
            let result = tokio::spawn({
                let speech = shared.speech.clone();
                async move { speech.synthesize(text, debug_name, cancel).await }
            })
            .await
            .unwrap_or(Err(SpeechServiceError::ProviderFailed));
            shared
                .send_connection_command(
                    &device_id,
                    &connection_id,
                    ConnectionCommand::SpeechTtsResult {
                        request_id,
                        stream_id,
                        result,
                    },
                )
                .await;
        }
    });
    Ok(())
}

async fn deliver_relay_tts_result(
    socket: &mut WebSocket,
    relay: &mut RelayState,
    request_id: String,
    stream_id: u32,
    result: Result<Arc<[u8]>, SpeechServiceError>,
) -> Result<(), ConnectionError> {
    if !relay.in_flight_tts.remove(&request_id) {
        return Ok(());
    }
    match result {
        Ok(pcm) if !pcm.is_empty() => {
            send_payload(
                socket,
                "speech_tts_start",
                &SpeechTtsStart {
                    request_id: request_id.clone(),
                    stream_id,
                    ok: true,
                    format: Some(PcmFormat {
                        encoding: "pcm_s16le".to_owned(),
                        sample_rate_hz: 16_000,
                        channels: 1,
                        frame_duration_ms: 0,
                    }),
                    total_bytes: Some(pcm.len() as u64),
                    error: None,
                },
            )
            .await?;
            for (sequence, chunk) in pcm.chunks(RELAY_DOWNLINK_CHUNK_BYTES).enumerate() {
                let frame = RelayDownlinkPcmFrame::encode(stream_id, sequence as u32, chunk)?;
                socket.send(Message::Binary(frame.into())).await?;
            }
            send_payload(
                socket,
                "speech_tts_end",
                &SpeechTtsEnd {
                    request_id,
                    stream_id,
                    ok: true,
                    error: None,
                },
            )
            .await
        }
        Ok(_) => send_speech_tts_error(socket, request_id, stream_id, "invalid_audio").await,
        Err(error) => send_speech_tts_error(socket, request_id, stream_id, error.code()).await,
    }
}

/// 一个已预留的播报队列项。
///
/// TTS 生成期间 `output` 为空；生成完成后再附加 PCM，并且只有队首会创建传输流。
struct ActivePlayback {
    prepared_at: Instant,
    output_id: String,
    cancellation: tokio_util::sync::CancellationToken,
    output: Option<super::gateway::PlaybackOutput>,
    stream: Option<PlaybackStream>,
}

/// 当前下行 PCM 流的发送游标和严格递增帧序号。
struct PlaybackStream {
    started_at: Instant,
    stream_id: u32,
    pcm: Arc<[u8]>,
    offset: usize,
    sequence: u32,
}

impl ActivePlayback {
    fn prepared(output_id: String, cancellation: tokio_util::sync::CancellationToken) -> Self {
        Self {
            prepared_at: Instant::now(),
            output_id,
            cancellation,
            output: None,
            stream: None,
        }
    }

    fn start(&mut self, stream_id: u32, pcm: Arc<[u8]>) {
        self.stream = Some(PlaybackStream {
            started_at: Instant::now(),
            stream_id,
            pcm,
            offset: 0,
            sequence: 0,
        });
    }

    fn stream_id(&self) -> Option<u32> {
        self.stream.as_ref().map(|stream| stream.stream_id)
    }
}

impl Drop for ActivePlayback {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

fn reserve_playback(
    playback: &mut VecDeque<ActivePlayback>,
    output_id: String,
    cancellation: tokio_util::sync::CancellationToken,
) -> bool {
    if playback.len() >= MAX_QUEUED_PLAYBACKS
        || playback.iter().any(|entry| entry.output_id == output_id)
    {
        return false;
    }
    playback.push_back(ActivePlayback::prepared(output_id, cancellation));
    if let Some(active) = playback.back() {
        log_playback_queue("reserved", &active.output_id, playback, "ok");
    }
    true
}

fn attach_playback_output(
    playback: &mut VecDeque<ActivePlayback>,
    output: super::gateway::PlaybackOutput,
) -> bool {
    let Some(queued) = playback.iter_mut().find(|queued| {
        queued.output_id == output.output_id
            && !queued.cancellation.is_cancelled()
            && queued.output.is_none()
            && queued.stream.is_none()
    }) else {
        return false;
    };
    queued.output = Some(output);
    true
}

/// 连接 owner 先附加 PCM 再确认；回执接收者已消失时不播放未确认的输出。
fn acknowledge_playback_output(
    playback: &mut VecDeque<ActivePlayback>,
    output: super::gateway::PlaybackOutput,
    response: tokio::sync::oneshot::Sender<bool>,
) {
    let output_id = output.output_id.clone();
    let accepted = !response.is_closed() && attach_playback_output(playback, output);
    log_playback_queue(
        "attached",
        &output_id,
        playback,
        if accepted { "ok" } else { "rejected" },
    );
    if response.send(accepted).is_err()
        && accepted
        && let Some(queued) = playback.iter().find(|queued| queued.output_id == output_id)
    {
        queued.cancellation.cancel();
    }
}

fn log_playback_queue(
    stage: &'static str,
    output_id: &str,
    playback: &VecDeque<ActivePlayback>,
    result: &str,
) {
    let pcm_bytes: usize = playback
        .iter()
        .map(|entry| {
            entry.output.as_ref().map_or(0, |output| output.pcm.len())
                + entry.stream.as_ref().map_or(0, |stream| stream.pcm.len())
        })
        .sum();
    eprintln!(
        "event=playback_queue ts_ms={} request={} stage={} depth={} pcm_bytes={} result={}",
        timestamp_ms(),
        correlation_id(output_id),
        stage,
        playback.len(),
        pcm_bytes,
        result
    );
}

async fn cancel_playback(
    socket: &mut WebSocket,
    playback: &mut VecDeque<ActivePlayback>,
    reason: &str,
) -> Result<(), ConnectionError> {
    let active_stream = playback.front().and_then(|active| {
        active
            .stream_id()
            .map(|stream_id| (active.output_id.clone(), stream_id))
    });
    for active in playback.drain(..) {
        eprintln!(
            "event=playback_discarded ts_ms={} request={} stream_id={} result={}",
            timestamp_ms(),
            correlation_id(&active.output_id),
            active.stream_id().unwrap_or(0),
            reason
        );
        active.cancellation.cancel();
    }
    if let Some((output_id, stream_id)) = active_stream {
        send_playback_end(socket, &output_id, stream_id, reason).await?;
    }
    Ok(())
}

async fn advance_playback(
    socket: &mut WebSocket,
    playback: &mut VecDeque<ActivePlayback>,
) -> Result<(), ConnectionError> {
    synchronize_playback(socket, playback).await?;
    if playback
        .front()
        .is_some_and(|active| active.stream.is_some())
    {
        send_next_playback_frame(socket, playback).await?;
    }
    Ok(())
}

async fn synchronize_playback(
    socket: &mut WebSocket,
    playback: &mut VecDeque<ActivePlayback>,
) -> Result<(), ConnectionError> {
    while playback
        .front()
        .is_some_and(|active| active.cancellation.is_cancelled())
    {
        let active = playback.pop_front().expect("front checked");
        log_playback_queue("discarded", &active.output_id, playback, "cancelled");
        if let Some(stream_id) = active.stream_id() {
            send_playback_end(socket, &active.output_id, stream_id, "cancelled").await?;
        }
    }
    let should_start = playback_should_start(playback);
    if should_start {
        let stream_id = random_stream_id()?;
        let output = playback
            .front_mut()
            .and_then(|active| active.output.take())
            .expect("ready front has output");
        send_payload(
            socket,
            "playback_start",
            &PlaybackStart {
                output_id: output.output_id.clone(),
                run_id: output.run_id,
                stream_id,
                format: protocol_pcm_format(),
                text: output.text,
                sample_count: u64::try_from(output.pcm.len() / 2)
                    .map_err(|_| ConnectionError::InvalidMessage)?,
            },
        )
        .await?;
        if let Some(active) = playback.front_mut() {
            eprintln!(
                "event=playback_started ts_ms={} request={} stream_id={} queue_ms={} pcm_bytes={}",
                timestamp_ms(),
                correlation_id(&active.output_id),
                stream_id,
                active.prepared_at.elapsed().as_millis(),
                output.pcm.len()
            );
            active.start(stream_id, output.pcm);
        }
    }
    Ok(())
}

fn playback_should_start(playback: &VecDeque<ActivePlayback>) -> bool {
    playback
        .front()
        .is_some_and(|active| active.stream.is_none() && active.output.is_some())
}

async fn send_next_playback_frame(
    socket: &mut WebSocket,
    playback: &mut VecDeque<ActivePlayback>,
) -> Result<(), ConnectionError> {
    let Some(active) = playback.front_mut() else {
        return Ok(());
    };
    let Some(stream) = active.stream.as_mut() else {
        return Ok(());
    };
    let remaining = stream.pcm.len().saturating_sub(stream.offset);
    let length = remaining.min(super::protocol::PCM_PAYLOAD_BYTES);
    let end = stream.offset.saturating_add(length);
    let frame = DownlinkPcmFrame::encode(
        stream.stream_id,
        stream.sequence,
        &stream.pcm[stream.offset..end],
    )?;
    let send_result = timeout(
        PLAYBACK_SEND_TIMEOUT,
        socket.send(Message::Binary(frame.into())),
    )
    .await;
    if !matches!(send_result, Ok(Ok(()))) {
        let output_id = active.output_id.clone();
        let stream_id = stream.stream_id;
        active.cancellation.cancel();
        playback.pop_front();
        log_playback_queue("finished", &output_id, playback, "backpressure");
        return send_playback_end(socket, &output_id, stream_id, "backpressure").await;
    }
    if stream.sequence == 0 {
        eprintln!(
            "event=playback_first_frame ts_ms={} request={} stream_id={} elapsed_ms={}",
            timestamp_ms(),
            correlation_id(&active.output_id),
            stream.stream_id,
            stream.started_at.elapsed().as_millis()
        );
    }
    stream.offset = end;
    stream.sequence = stream
        .sequence
        .checked_add(1)
        .ok_or(ConnectionError::InvalidMessage)?;
    if stream.offset == stream.pcm.len() {
        let output_id = active.output_id.clone();
        let stream_id = stream.stream_id;
        eprintln!(
            "event=playback_sent ts_ms={} request={} stream_id={} frames={} pcm_bytes={} elapsed_ms={} result=completed",
            timestamp_ms(),
            correlation_id(&output_id),
            stream_id,
            stream.sequence,
            stream.pcm.len(),
            stream.started_at.elapsed().as_millis()
        );
        playback.pop_front();
        log_playback_queue("finished", &output_id, playback, "completed");
        send_playback_end(socket, &output_id, stream_id, "completed").await?;
    }
    Ok(())
}

async fn send_playback_end(
    socket: &mut WebSocket,
    output_id: &str,
    stream_id: u32,
    reason: &str,
) -> Result<(), ConnectionError> {
    timeout(
        PLAYBACK_SEND_TIMEOUT,
        send_payload(
            socket,
            "playback_end",
            &PlaybackEnd {
                output_id: output_id.to_owned(),
                stream_id,
                reason: reason.to_owned(),
            },
        ),
    )
    .await
    .map_err(|_| ConnectionError::PlaybackBackpressure)??;
    Ok(())
}

fn protocol_pcm_format() -> PcmFormat {
    PcmFormat {
        encoding: "pcm_s16le".to_owned(),
        sample_rate_hz: 16_000,
        channels: 1,
        frame_duration_ms: 20,
    }
}

/// 当前正在接收的一段上行 PCM 及其顺序校验状态。
struct UplinkUtterance {
    client_input_id: String,
    stream_id: u32,
    output_preference: OutputPreferenceSnapshot,
    next_sequence: u32,
    pcm: Vec<u8>,
}

impl UplinkUtterance {
    fn new(request: ListenStart) -> Self {
        Self {
            client_input_id: request.client_input_id,
            stream_id: request.stream_id,
            output_preference: request.output_preference,
            next_sequence: 0,
            pcm: Vec::new(),
        }
    }

    fn push(&mut self, frame: UplinkPcmFrame<'_>) -> Result<(), UplinkError> {
        if frame.stream_id != self.stream_id {
            return Err(UplinkError::WrongStream);
        }
        if frame.sequence != self.next_sequence {
            return Err(UplinkError::Sequence);
        }
        if self.pcm.len().saturating_add(frame.payload.len()) > MAX_UPLINK_PCM_BYTES {
            return Err(UplinkError::TooLarge);
        }
        self.pcm.extend_from_slice(frame.payload);
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(UplinkError::TooLarge)?;
        Ok(())
    }

    fn last_sequence(&self) -> Option<u32> {
        self.next_sequence.checked_sub(1)
    }
}

/// 上行 PCM 帧不能并入当前段的原因。
#[derive(Clone, Copy, Debug)]
enum UplinkError {
    WrongStream,
    Sequence,
    TooLarge,
}

/// 一个异步 ASR 子任务的完成值；`ordinal` 用于恢复设备原始分段顺序。
struct RecognitionOutcome {
    ordinal: usize,
    stream_id: u32,
    client_input_id: String,
    result: Result<String, SpeechServiceError>,
}

/// 同一设备一次逻辑语音输入跨多个音频段、甚至跨 WSS 连接的聚合状态。
///
/// 只有全部已接管段识别完成并经过短暂收口期后，才合并为一个 Runtime Input。
pub(super) struct VoiceTurnAggregation {
    started_at: Instant,
    logical_client_input_id: String,
    output_preference: OutputPreferenceSnapshot,
    next_ordinal: usize,
    pending_recognitions: usize,
    transcripts: BTreeMap<usize, String>,
    segment_fingerprints: HashMap<String, [u8; 32]>,
    recognition_cancellations: HashMap<u32, tokio_util::sync::CancellationToken>,
    completed_recognitions: VecDeque<RecognitionOutcome>,
    commit_deadline: Option<TokioInstant>,
}

/// 一段音频进入当前逻辑语音轮次时的去重判定。
enum SegmentAdmission {
    New {
        ordinal: usize,
        cancellation: tokio_util::sync::CancellationToken,
    },
    Duplicate,
}

impl VoiceTurnAggregation {
    pub(super) fn cancel(&mut self) {
        for cancellation in self.recognition_cancellations.values() {
            cancellation.cancel();
        }
    }

    fn new(logical_client_input_id: String, output_preference: OutputPreferenceSnapshot) -> Self {
        Self {
            started_at: Instant::now(),
            logical_client_input_id,
            output_preference,
            next_ordinal: 0,
            pending_recognitions: 0,
            transcripts: BTreeMap::new(),
            segment_fingerprints: HashMap::new(),
            recognition_cancellations: HashMap::new(),
            completed_recognitions: VecDeque::new(),
            commit_deadline: None,
        }
    }

    fn admit_segment(
        &mut self,
        client_input_id: String,
        fingerprint: [u8; 32],
        stream_id: u32,
    ) -> Option<SegmentAdmission> {
        if let Some(existing) = self.segment_fingerprints.get(&client_input_id) {
            return (*existing == fingerprint).then_some(SegmentAdmission::Duplicate);
        }
        if self.segment_count() >= MAX_VOICE_TURN_SEGMENTS {
            return None;
        }
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        self.pending_recognitions += 1;
        self.segment_fingerprints
            .insert(client_input_id, fingerprint);
        let cancellation = tokio_util::sync::CancellationToken::new();
        self.recognition_cancellations
            .insert(stream_id, cancellation.clone());
        Some(SegmentAdmission::New {
            ordinal,
            cancellation,
        })
    }

    fn complete_segment(&mut self, outcome: RecognitionOutcome) {
        self.pending_recognitions = self.pending_recognitions.saturating_sub(1);
        self.recognition_cancellations.remove(&outcome.stream_id);
        if let Ok(transcript) = &outcome.result {
            self.transcripts.insert(outcome.ordinal, transcript.clone());
        }
        self.completed_recognitions.push_back(outcome);
    }

    fn cancellation(&self, stream_id: u32) -> Option<tokio_util::sync::CancellationToken> {
        self.recognition_cancellations.get(&stream_id).cloned()
    }

    fn pause_commit(&mut self) {
        self.commit_deadline = None;
    }

    fn schedule_commit(&mut self) {
        self.commit_deadline = Some(TokioInstant::now() + VOICE_TURN_COMMIT_DELAY);
    }

    fn ready_to_commit(&self) -> bool {
        self.pending_recognitions == 0
            && self
                .commit_deadline
                .is_some_and(|deadline| deadline <= TokioInstant::now())
    }

    fn segment_count(&self) -> usize {
        self.next_ordinal
    }

    fn can_start_segment(
        &self,
        client_input_id: &str,
        output_preference: OutputPreferenceSnapshot,
    ) -> bool {
        self.output_preference == output_preference
            && (self.segment_count() < MAX_VOICE_TURN_SEGMENTS
                || self.segment_fingerprints.contains_key(client_input_id))
    }

    fn is_empty(&self) -> bool {
        self.next_ordinal == 0
    }

    fn merged_transcript(&self) -> Result<Option<String>, ()> {
        if self.transcripts.is_empty() {
            return Ok(None);
        }
        let mut merged = String::new();
        for transcript in self.transcripts.values() {
            if !merged.is_empty() {
                merged.push('\n');
            }
            if merged.len().saturating_add(transcript.len()) > MAX_TEXT_INPUT_BYTES {
                return Err(());
            }
            merged.push_str(transcript);
        }
        Ok(Some(merged))
    }
}

/// 当前尚未接管的录音段结束后恢复 logical voice turn 的收口时钟。
///
/// 空聚合没有任何可提交事实，直接清除；已有分段则重新启动 commit deadline。该转换只修改
/// Gateway 的易失聚合状态，不提交 Runtime Input，也不取消已经运行的 ASR。
fn resume_or_clear_voice_turn(aggregation: &mut Option<VoiceTurnAggregation>) {
    if aggregation
        .as_ref()
        .is_some_and(VoiceTurnAggregation::is_empty)
    {
        *aggregation = None;
    } else if let Some(aggregation) = aggregation.as_mut() {
        aggregation.schedule_commit();
    }
}

async fn process_voice_turn(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    device_id: &DeviceId,
    capabilities: DeviceCapabilitiesSnapshot,
    voice_turn: &Arc<tokio::sync::Mutex<Option<VoiceTurnAggregation>>>,
) -> Result<(), ConnectionError> {
    let (outcomes, completed) = {
        let mut aggregation = voice_turn.lock().await;
        let outcomes: Vec<RecognitionOutcome> = aggregation
            .as_mut()
            .map(|aggregation| aggregation.completed_recognitions.drain(..).collect())
            .unwrap_or_default();
        let completed = aggregation
            .as_ref()
            .is_some_and(VoiceTurnAggregation::ready_to_commit)
            .then(|| aggregation.take())
            .flatten();
        (outcomes, completed)
    };
    for outcome in outcomes {
        eprintln!(
            "event=voice_segment_finished ts_ms={} device={} input={} stream_id={} result={}",
            timestamp_ms(),
            correlation_id(device_id.as_str()),
            correlation_id(&outcome.client_input_id),
            outcome.stream_id,
            outcome
                .result
                .as_ref()
                .err()
                .map_or("ok", SpeechServiceError::code)
        );
        match outcome.result {
            Ok(recognized) => {
                if capabilities.display_transcript {
                    send_payload(
                        socket,
                        "transcript",
                        &Transcript {
                            client_input_id: outcome.client_input_id,
                            text: recognized,
                        },
                    )
                    .await?;
                }
            }
            Err(error) => {
                let (code, recoverable) = speech_error(error);
                send_wire_error(socket, code, None, recoverable).await?;
            }
        }
    }
    let Some(completed) = completed else {
        return Ok(());
    };
    eprintln!(
        "event=voice_turn_ready ts_ms={} device={} input={} segments={} recognized={} elapsed_ms={}",
        timestamp_ms(),
        correlation_id(device_id.as_str()),
        correlation_id(&completed.logical_client_input_id),
        completed.segment_count(),
        completed.transcripts.len(),
        completed.started_at.elapsed().as_millis()
    );
    let transcript = match completed.merged_transcript() {
        Ok(Some(transcript)) => transcript,
        Ok(None) => {
            send_payload(
                socket,
                "state_changed",
                &InteractionStateChanged {
                    run_id: None,
                    client_input_id: Some(completed.logical_client_input_id),
                    state: "idle".to_owned(),
                    reason: Some("no_speech_recognized".to_owned()),
                },
            )
            .await?;
            return Ok(());
        }
        Err(()) => {
            send_wire_error(socket, "voice_turn_too_large", None, true).await?;
            send_payload(
                socket,
                "state_changed",
                &InteractionStateChanged {
                    run_id: None,
                    client_input_id: Some(completed.logical_client_input_id),
                    state: "idle".to_owned(),
                    reason: Some("voice_turn_too_large".to_owned()),
                },
            )
            .await?;
            return Ok(());
        }
    };
    let submission_started = Instant::now();
    match submit_speech_input(
        shared.runtime.as_ref(),
        device_id,
        &completed.logical_client_input_id,
        transcript,
        completed.output_preference,
    )
    .await
    {
        Ok(accepted) => {
            eprintln!(
                "event=voice_input_accepted ts_ms={} device={} input={} run={} elapsed_ms={}",
                timestamp_ms(),
                correlation_id(device_id.as_str()),
                correlation_id(&completed.logical_client_input_id),
                accepted.run_id.as_ref().map_or_else(
                    || "none".to_owned(),
                    |run_id| correlation_id(run_id.as_str())
                ),
                submission_started.elapsed().as_millis()
            );
            send_payload(socket, "input_accepted", &accepted).await?;
        }
        Err(error) => {
            let (code, recoverable) = input_error(&error);
            eprintln!(
                "event=voice_input_rejected ts_ms={} input={} elapsed_ms={} result={}",
                timestamp_ms(),
                correlation_id(&completed.logical_client_input_id),
                submission_started.elapsed().as_millis(),
                code
            );
            send_wire_error(socket, code, None, recoverable).await?;
            send_payload(
                socket,
                "state_changed",
                &InteractionStateChanged {
                    run_id: None,
                    client_input_id: Some(completed.logical_client_input_id),
                    state: "idle".to_owned(),
                    reason: Some(code.to_owned()),
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn recognize_segment(
    speech: crate::speech::SpeechServiceHandle,
    device_id: DeviceId,
    utterance: UplinkUtterance,
    ordinal: usize,
    cancellation: tokio_util::sync::CancellationToken,
) -> RecognitionOutcome {
    let client_input_id = utterance.client_input_id.clone();
    let stream_id = utterance.stream_id;
    let debug_name = format!(
        "{}-{}-{}",
        now_ms().unwrap_or_default(),
        device_id.as_str().chars().take(12).collect::<String>(),
        client_input_id.chars().take(24).collect::<String>()
    );
    eprintln!(
        "event=voice_segment_started ts_ms={} device={} input={} request={} stream_id={} pcm_bytes={}",
        timestamp_ms(),
        correlation_id(device_id.as_str()),
        correlation_id(&client_input_id),
        correlation_id(&debug_name),
        stream_id,
        utterance.pcm.len()
    );
    let result = match speech
        .recognize(utterance.pcm, debug_name, cancellation)
        .await
    {
        Ok(transcript) => {
            let transcript = transcript.trim().to_owned();
            if transcript.is_empty() || transcript.len() > MAX_TEXT_INPUT_BYTES {
                Err(SpeechServiceError::InvalidTranscript)
            } else {
                Ok(transcript)
            }
        }
        Err(error) => Err(error),
    };
    RecognitionOutcome {
        ordinal,
        stream_id,
        client_input_id,
        result,
    }
}

fn spawn_segment_recognition(
    shared: &GatewayShared,
    device_id: DeviceId,
    utterance: UplinkUtterance,
    ordinal: usize,
    cancellation: tokio_util::sync::CancellationToken,
    voice_turn: Arc<tokio::sync::Mutex<Option<VoiceTurnAggregation>>>,
    logical_client_input_id: String,
) {
    let failed_outcome = RecognitionOutcome {
        ordinal,
        stream_id: utterance.stream_id,
        client_input_id: utterance.client_input_id.clone(),
        result: Err(SpeechServiceError::Unavailable),
    };
    let recognition = recognize_segment(
        shared.speech.clone(),
        device_id,
        utterance,
        ordinal,
        cancellation,
    );
    spawn_owned_recognition(
        &shared.recognition_tasks,
        recognition,
        failed_outcome,
        voice_turn,
        logical_client_input_id,
    );
}

/// 将一段识别登记到 Gateway 任务树，并把所有退出路径收敛成聚合 outcome。
///
/// 外层 wrapper 由 Gateway `TaskTracker` 拥有；内层 `JoinHandle` 只隔离单次识别的 panic。
/// 无论识别成功、返回错误还是异常退出，只要原 logical turn 仍然有效就精确减少一次 pending；
/// 本函数不提交 Runtime Input，也不改变跨连接聚合的 commit deadline。
fn spawn_owned_recognition(
    tasks: &tokio_util::task::TaskTracker,
    recognition: impl Future<Output = RecognitionOutcome> + Send + 'static,
    failed_outcome: RecognitionOutcome,
    voice_turn: Arc<tokio::sync::Mutex<Option<VoiceTurnAggregation>>>,
    logical_client_input_id: String,
) {
    tasks.spawn(async move {
        let outcome = match tokio::spawn(recognition).await {
            Ok(outcome) => outcome,
            Err(_) => failed_outcome,
        };
        settle_recognition_outcome(&voice_turn, &logical_client_input_id, outcome).await;
    });
}

async fn settle_recognition_outcome(
    voice_turn: &Arc<tokio::sync::Mutex<Option<VoiceTurnAggregation>>>,
    logical_client_input_id: &str,
    outcome: RecognitionOutcome,
) {
    let mut aggregation = voice_turn.lock().await;
    if let Some(active) = aggregation.as_mut()
        && logical_client_input_id == active.logical_client_input_id
    {
        active.complete_segment(outcome);
    }
}

async fn cancel_active_controller_run(
    runtime: &assistant_runtime::AssistantRuntime,
) -> Result<bool, assistant_runtime::RuntimeError> {
    let session = route_device_channel(runtime).await?;
    let session_id = session.session_id;
    let Some(run_id) = session.active_run_id else {
        return Ok(false);
    };
    runtime
        .cancel_run(CancelRunRequest { session_id, run_id })
        .await?;
    Ok(true)
}

async fn submit_speech_input(
    runtime: &assistant_runtime::AssistantRuntime,
    device_id: &DeviceId,
    client_input_id: &str,
    transcript: String,
    output_preference: OutputPreferenceSnapshot,
) -> Result<InputAccepted, assistant_runtime::RuntimeError> {
    let target = route_device_channel(runtime).await?;
    let session_id = target.session_id;
    let variant = target.current_variant;
    let result = runtime
        .submit_session_input(SubmitSessionInputRequest {
            input: SubmitInputRequest {
                delivery: Default::default(),
                session_id,
                message: transcript,
                variant,
                mode: SubmitInputMode::Normal,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            },
            source: InputChannelSource::Device(DeviceInputSource {
                device_id: device_id.clone(),
                client_input_id: client_input_id.to_owned(),
                modality: InputModality::SpeechTranscript,
                requested_output: output_preference_from_snapshot(output_preference),
            }),
        })
        .await?;
    Ok(InputAccepted::from_submission(
        client_input_id.to_owned(),
        result,
    ))
}

fn validate_listen_start(request: &ListenStart) -> Result<(), ConnectionError> {
    if request.client_input_id.trim().is_empty()
        || request.client_input_id.len() > MAX_CLIENT_INPUT_ID_BYTES
        || request.stream_id == 0
        || !request.format.is_protocol_v1()
    {
        return Err(ConnectionError::InvalidMessage);
    }
    Ok(())
}

fn input_fingerprint(
    modality: &[u8],
    content: &[u8],
    preference: OutputPreferenceSnapshot,
) -> [u8; 32] {
    let preference = match preference {
        OutputPreferenceSnapshot::Text => 1,
        OutputPreferenceSnapshot::Audio => 2,
        OutputPreferenceSnapshot::TextAndAudio => 3,
    };
    let mut digest = Sha256::new();
    digest.update(modality);
    digest.update([0]);
    digest.update([preference]);
    digest.update(content);
    digest.finalize().into()
}

/// 单连接最近客户端输入的有界指纹窗口。
///
/// 相同 ID/相同内容视为安全重试；相同 ID/不同内容会被拒绝，避免静默改写输入。
#[derive(Default)]
struct ClientInputWindow {
    fingerprints: HashMap<String, [u8; 32]>,
    order: VecDeque<String>,
}

impl ClientInputWindow {
    fn remember(&mut self, client_input_id: &str, fingerprint: [u8; 32]) -> bool {
        match self.fingerprints.get(client_input_id) {
            Some(existing) => existing == &fingerprint,
            None => {
                self.fingerprints
                    .insert(client_input_id.to_owned(), fingerprint);
                self.order.push_back(client_input_id.to_owned());
                if self.order.len() > MAX_CLIENT_INPUT_FINGERPRINTS
                    && let Some(expired) = self.order.pop_front()
                {
                    self.fingerprints.remove(&expired);
                }
                true
            }
        }
    }
}

fn speech_error(error: SpeechServiceError) -> (&'static str, bool) {
    match error {
        SpeechServiceError::Unavailable | SpeechServiceError::Busy => ("asr_unavailable", true),
        SpeechServiceError::InvalidInput => ("asr_provider_failed", false),
        SpeechServiceError::Cancelled => ("asr_cancelled", true),
        SpeechServiceError::Authentication => ("asr_auth_failed", true),
        SpeechServiceError::Timeout => ("asr_timeout", true),
        SpeechServiceError::InvalidTranscript => ("asr_empty_transcript", true),
        SpeechServiceError::InvalidAudio | SpeechServiceError::OutputTooLarge => {
            ("asr_provider_failed", true)
        }
        SpeechServiceError::ProviderFailed => ("asr_provider_failed", true),
    }
}

async fn submit_text_input(
    shared: &GatewayShared,
    device_id: &DeviceId,
    request: TextInput,
) -> Result<InputAccepted, assistant_runtime::RuntimeError> {
    let target = route_device_channel(shared.runtime.as_ref()).await?;
    let session_id = target.session_id;
    let variant = target.current_variant;
    let result = shared
        .runtime
        .submit_session_input(SubmitSessionInputRequest {
            input: SubmitInputRequest {
                delivery: Default::default(),
                session_id,
                message: request.text,
                variant,
                mode: SubmitInputMode::Normal,
                attachment_ids: Vec::new(),
                quotes: Vec::new(),
                skill_name: None,
                mcp_server_key: None,
                idempotency_key: None,
            },
            source: InputChannelSource::Device(DeviceInputSource {
                device_id: device_id.clone(),
                client_input_id: request.client_input_id.clone(),
                modality: InputModality::Text,
                requested_output: output_preference_from_snapshot(request.output_preference),
            }),
        })
        .await?;
    Ok(InputAccepted::from_submission(
        request.client_input_id,
        result,
    ))
}

/// 本版本 Device Channel 的 Host 路由策略：选择当前活动 Controller Session。
///
/// Router 每次读取 Runtime 权威 Session 投影，不在 Gateway 内缓存 Session ID 或角色状态。
async fn route_device_channel(
    runtime: &assistant_runtime::AssistantRuntime,
) -> Result<assistant_protocol::SessionSummary, assistant_runtime::RuntimeError> {
    runtime.controller_summary().await
}

fn validate_text_input(request: &TextInput) -> Result<(), ConnectionError> {
    if request.client_input_id.trim().is_empty()
        || request.client_input_id.len() > MAX_CLIENT_INPUT_ID_BYTES
        || request.text.trim().is_empty()
        || request.text.len() > MAX_TEXT_INPUT_BYTES
    {
        return Err(ConnectionError::InvalidMessage);
    }
    Ok(())
}

fn output_preference_from_snapshot(preference: OutputPreferenceSnapshot) -> OutputPreference {
    match preference {
        OutputPreferenceSnapshot::Text => OutputPreference::Text,
        OutputPreferenceSnapshot::Audio => OutputPreference::Audio,
        OutputPreferenceSnapshot::TextAndAudio => OutputPreference::TextAndAudio,
    }
}

fn input_error(error: &assistant_runtime::RuntimeError) -> (&'static str, bool) {
    match error {
        assistant_runtime::RuntimeError::ControllerUnavailable => ("controller_unavailable", true),
        assistant_runtime::RuntimeError::RuntimeNotRunning { .. } => ("runtime_unavailable", true),
        _ => ("input_rejected", true),
    }
}

async fn receive_typed<T: serde::de::DeserializeOwned>(
    socket: &mut WebSocket,
    replay: &mut MessageReplayWindow,
    timeout: Duration,
    expected_type: &str,
) -> Result<T, ConnectionError> {
    let envelope = receive_envelope(socket, replay, timeout).await?;
    if envelope.message_type != expected_type {
        return Err(ConnectionError::InvalidMessage);
    }
    Ok(envelope.payload::<T>()?)
}

async fn receive_envelope(
    socket: &mut WebSocket,
    replay: &mut MessageReplayWindow,
    timeout: Duration,
) -> Result<Envelope, ConnectionError> {
    let message = tokio::time::timeout(timeout, socket.next())
        .await
        .map_err(|_| ConnectionError::HandshakeTimedOut)?
        .ok_or(ConnectionError::Closed)??;
    let Message::Text(text) = message else {
        return Err(ConnectionError::InvalidMessage);
    };
    let envelope = Envelope::decode(&text)?;
    replay.accept(&envelope.message_id)?;
    Ok(envelope)
}

async fn send_payload<T: serde::Serialize>(
    socket: &mut WebSocket,
    message_type: &str,
    payload: &T,
) -> Result<(), ConnectionError> {
    let envelope = Envelope::new(random_token(12)?, message_type, payload)?;
    socket
        .send(Message::Text(envelope.encode()?.into()))
        .await?;
    Ok(())
}

async fn send_wire_error(
    socket: &mut WebSocket,
    code: &str,
    correlation_message_id: Option<String>,
    recoverable: bool,
) -> Result<(), ConnectionError> {
    send_payload(
        socket,
        "error",
        &WireError {
            code: code.to_owned(),
            correlation_message_id,
            recoverable,
        },
    )
    .await
}

async fn reject_authentication(socket: &mut WebSocket) -> Result<(), ConnectionError> {
    send_wire_error(socket, "authentication_failed", None, false).await?;
    close(socket, 1008, "authentication failed").await
}

async fn reject_revoked_device(socket: &mut WebSocket) -> Result<(), ConnectionError> {
    send_wire_error(socket, "device_revoked", None, false).await?;
    close(socket, 1008, "device revoked").await
}

async fn close(
    socket: &mut WebSocket,
    code: u16,
    reason: &'static str,
) -> Result<(), ConnectionError> {
    socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await?;
    Ok(())
}

fn validate_pairing_hello(hello: &PairingHello) -> Result<(), ConnectionError> {
    if hello.pairing_request_id.trim().is_empty()
        || hello.pairing_request_id.len() > 128
        || hello.display_name.trim().is_empty()
        || hello.display_name.len() > MAX_DEVICE_NAME_BYTES
        || hello.device_nonce.trim().is_empty()
        || hello.device_nonce.len() > MAX_NONCE_BYTES
        || hello.client_version.as_deref().is_some_and(|version| {
            version.trim().is_empty() || version.len() > MAX_CLIENT_VERSION_BYTES
        })
    {
        return Err(ConnectionError::InvalidMessage);
    }
    Ok(())
}

fn validate_device_hello(hello: &DeviceHello) -> Result<(), ConnectionError> {
    if hello.device_id.trim().is_empty()
        || hello.device_id.len() > 256
        || hello.device_nonce.trim().is_empty()
        || hello.device_nonce.len() > MAX_NONCE_BYTES
        || hello.client_version.trim().is_empty()
        || hello.client_version.len() > MAX_CLIENT_VERSION_BYTES
    {
        return Err(ConnectionError::InvalidMessage);
    }
    Ok(())
}

fn ensure_pairing_request(actual: &str, expected: &str) -> Result<(), ConnectionError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ConnectionError::InvalidMessage)
    }
}

fn effective_capabilities(
    declared: DeviceCapabilitiesSnapshot,
    asr_ready: bool,
    tts_ready: bool,
) -> DeviceCapabilitiesSnapshot {
    DeviceCapabilitiesSnapshot {
        input_text: declared.input_text,
        input_pcm16_16k_mono: declared.input_pcm16_16k_mono && asr_ready,
        output_text: declared.output_text,
        output_pcm16_16k_mono: declared.output_pcm16_16k_mono && tts_ready,
        playback_cancel: declared.playback_cancel && declared.output_pcm16_16k_mono && tts_ready,
        display_status: declared.display_status,
        display_transcript: declared.display_transcript,
    }
}

fn now_ms() -> Result<i64, ConnectionError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ConnectionError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| ConnectionError::Clock)
}

/// 单条设备连接的握手、协议、播放和 Runtime 桥接错误。
///
/// 发送给设备前仍需映射为有限 wire error，不能直接暴露本枚举文本。
#[derive(Debug, thiserror::Error)]
enum ConnectionError {
    #[error("device connection closed")]
    Closed,
    #[error("device handshake timed out")]
    HandshakeTimedOut,
    #[error("device pairing expired")]
    PairingExpired,
    #[error("device pairing was cancelled")]
    PairingCancelled,
    #[error("device pairing code did not match")]
    Spake2CodeMismatch,
    #[error("device control message is invalid")]
    InvalidMessage,
    #[error("system clock is unavailable")]
    Clock,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Crypto(#[from] super::crypto::CryptoError),
    #[error(transparent)]
    Gateway(#[from] DeviceGatewayError),
    #[error("device websocket failed: {0}")]
    WebSocket(#[from] axum::Error),
    #[error("device playback is backpressured")]
    PlaybackBackpressure,
    #[error("runtime device operation failed: {0}")]
    Runtime(#[from] assistant_runtime::RuntimeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_input_capability_requires_ready_host_asr() {
        let declared = DeviceCapabilitiesSnapshot {
            input_text: true,
            input_pcm16_16k_mono: true,
            output_text: true,
            output_pcm16_16k_mono: true,
            playback_cancel: true,
            display_status: true,
            display_transcript: true,
        };
        let effective = effective_capabilities(declared, false, false);
        assert!(effective.input_text);
        assert!(effective.output_text);
        assert!(!effective.input_pcm16_16k_mono);
        assert!(!effective.output_pcm16_16k_mono);
        assert!(!effective.playback_cancel);
        let effective = effective_capabilities(declared, true, true);
        assert!(effective.input_pcm16_16k_mono);
        assert!(effective.output_pcm16_16k_mono);
        assert!(effective.playback_cancel);
    }

    #[test]
    fn uplink_collector_enforces_stream_sequence_and_limit() {
        let mut utterance = UplinkUtterance::new(ListenStart {
            client_input_id: "client-input".to_owned(),
            stream_id: 7,
            format: super::super::protocol::PcmFormat {
                encoding: "pcm_s16le".to_owned(),
                sample_rate_hz: 16_000,
                channels: 1,
                frame_duration_ms: 20,
            },
            output_preference: OutputPreferenceSnapshot::Text,
            speaker_id: None,
            speaker_role: None,
            speaker_score: None,
            wake_source: None,
        });
        let payload = [0_u8; super::super::protocol::PCM_PAYLOAD_BYTES];
        utterance
            .push(UplinkPcmFrame {
                stream_id: 7,
                sequence: 0,
                payload: &payload,
            })
            .expect("first frame");
        assert_eq!(utterance.last_sequence(), Some(0));
        assert!(matches!(
            utterance.push(UplinkPcmFrame {
                stream_id: 7,
                sequence: 2,
                payload: &payload,
            }),
            Err(UplinkError::Sequence)
        ));
        assert!(matches!(
            utterance.push(UplinkPcmFrame {
                stream_id: 8,
                sequence: 1,
                payload: &payload,
            }),
            Err(UplinkError::WrongStream)
        ));
    }

    #[test]
    fn client_input_identity_can_retry_only_the_same_modality_content_and_preference() {
        let mut inputs = ClientInputWindow::default();
        let first = input_fingerprint(b"speech", &[1, 2], OutputPreferenceSnapshot::Text);
        assert!(inputs.remember("input", first));
        assert!(inputs.remember("input", first));
        assert!(!inputs.remember(
            "input",
            input_fingerprint(b"speech", &[1, 3], OutputPreferenceSnapshot::Text)
        ));
        assert!(!inputs.remember(
            "input",
            input_fingerprint(b"text", &[1, 2], OutputPreferenceSnapshot::Text)
        ));
    }

    #[test]
    fn voice_turn_merges_segments_in_capture_order_after_all_recognitions_finish() {
        let mut aggregation =
            VoiceTurnAggregation::new("logical-input".to_owned(), OutputPreferenceSnapshot::Audio);
        let SegmentAdmission::New { ordinal: first, .. } = aggregation
            .admit_segment("first".to_owned(), [1; 32], 1)
            .expect("first segment")
        else {
            panic!("first segment must be new");
        };
        let SegmentAdmission::New {
            ordinal: second, ..
        } = aggregation
            .admit_segment("second".to_owned(), [2; 32], 2)
            .expect("second segment")
        else {
            panic!("second segment must be new");
        };
        aggregation.schedule_commit();
        aggregation.commit_deadline = Some(TokioInstant::now());

        aggregation.complete_segment(RecognitionOutcome {
            ordinal: second,
            stream_id: 2,
            client_input_id: "second".to_owned(),
            result: Ok("第二段".to_owned()),
        });
        assert!(!aggregation.ready_to_commit());
        aggregation.complete_segment(RecognitionOutcome {
            ordinal: first,
            stream_id: 1,
            client_input_id: "first".to_owned(),
            result: Ok("第一段".to_owned()),
        });

        assert!(aggregation.ready_to_commit());
        assert_eq!(
            aggregation
                .merged_transcript()
                .expect("bounded transcript")
                .as_deref(),
            Some("第一段\n第二段")
        );
    }

    #[test]
    fn voice_turn_pause_cancels_the_pending_commit_and_empty_segments_are_omitted() {
        let mut aggregation =
            VoiceTurnAggregation::new("logical-input".to_owned(), OutputPreferenceSnapshot::Text);
        let SegmentAdmission::New { ordinal: empty, .. } = aggregation
            .admit_segment("empty".to_owned(), [3; 32], 3)
            .expect("empty segment")
        else {
            panic!("empty segment must be new");
        };
        aggregation.schedule_commit();
        aggregation.pause_commit();
        aggregation.complete_segment(RecognitionOutcome {
            ordinal: empty,
            stream_id: 3,
            client_input_id: "empty".to_owned(),
            result: Err(SpeechServiceError::InvalidTranscript),
        });

        assert!(!aggregation.ready_to_commit());
        assert_eq!(aggregation.merged_transcript(), Ok(None));
    }

    #[test]
    fn cancelled_uncommitted_segment_clears_empty_turn_or_resumes_existing_commit() {
        let mut empty = Some(VoiceTurnAggregation::new(
            "empty".to_owned(),
            OutputPreferenceSnapshot::Text,
        ));
        resume_or_clear_voice_turn(&mut empty);
        assert!(empty.is_none());

        let mut existing =
            VoiceTurnAggregation::new("logical-input".to_owned(), OutputPreferenceSnapshot::Text);
        assert!(matches!(
            existing.admit_segment("first".to_owned(), [9; 32], 9),
            Some(SegmentAdmission::New { .. })
        ));
        existing.pause_commit();
        let mut existing = Some(existing);
        resume_or_clear_voice_turn(&mut existing);
        assert!(
            existing
                .as_ref()
                .and_then(|aggregation| aggregation.commit_deadline)
                .is_some()
        );
    }

    #[tokio::test]
    async fn recognition_task_owner_settles_cancelled_and_panicked_segments() {
        let mut aggregation =
            VoiceTurnAggregation::new("logical-input".to_owned(), OutputPreferenceSnapshot::Text);
        let Some(SegmentAdmission::New {
            cancellation: cancelled_segment,
            ..
        }) = aggregation.admit_segment("cancelled".to_owned(), [10; 32], 10)
        else {
            panic!("cancelled segment must be admitted");
        };
        assert!(matches!(
            aggregation.admit_segment("panicked".to_owned(), [11; 32], 11),
            Some(SegmentAdmission::New { .. })
        ));
        let voice_turn = Arc::new(tokio::sync::Mutex::new(Some(aggregation)));
        let tasks = tokio_util::task::TaskTracker::new();
        let cancellation_wait = cancelled_segment.clone();

        spawn_owned_recognition(
            &tasks,
            async move {
                cancellation_wait.cancelled().await;
                RecognitionOutcome {
                    ordinal: 0,
                    stream_id: 10,
                    client_input_id: "cancelled".to_owned(),
                    result: Err(SpeechServiceError::Cancelled),
                }
            },
            RecognitionOutcome {
                ordinal: 0,
                stream_id: 10,
                client_input_id: "cancelled".to_owned(),
                result: Err(SpeechServiceError::Unavailable),
            },
            voice_turn.clone(),
            "logical-input".to_owned(),
        );
        spawn_owned_recognition(
            &tasks,
            async { panic!("injected recognition panic") },
            RecognitionOutcome {
                ordinal: 1,
                stream_id: 11,
                client_input_id: "panicked".to_owned(),
                result: Err(SpeechServiceError::Unavailable),
            },
            voice_turn.clone(),
            "logical-input".to_owned(),
        );

        voice_turn
            .lock()
            .await
            .as_mut()
            .expect("active voice turn")
            .cancel();
        tasks.close();
        tasks.wait().await;
        assert!(tasks.is_closed());
        assert!(tasks.is_empty());
        let aggregation = voice_turn.lock().await;
        let aggregation = aggregation.as_ref().expect("active voice turn");
        assert_eq!(aggregation.pending_recognitions, 0);
        assert!(aggregation.recognition_cancellations.is_empty());
        assert_eq!(aggregation.completed_recognitions.len(), 2);
        assert!(aggregation.completed_recognitions.iter().any(|outcome| {
            outcome.stream_id == 10 && outcome.result == Err(SpeechServiceError::Cancelled)
        }));
        assert!(aggregation.completed_recognitions.iter().any(|outcome| {
            outcome.stream_id == 11 && outcome.result == Err(SpeechServiceError::Unavailable)
        }));
    }

    #[test]
    fn voice_turn_accepts_an_identical_segment_retry_without_duplicate_asr() {
        let mut aggregation =
            VoiceTurnAggregation::new("logical-input".to_owned(), OutputPreferenceSnapshot::Text);
        assert!(matches!(
            aggregation.admit_segment("segment".to_owned(), [7; 32], 41),
            Some(SegmentAdmission::New { .. })
        ));
        assert!(matches!(
            aggregation.admit_segment("segment".to_owned(), [7; 32], 99),
            Some(SegmentAdmission::Duplicate)
        ));
        assert!(
            aggregation
                .admit_segment("segment".to_owned(), [8; 32], 99)
                .is_none()
        );
        assert_eq!(aggregation.segment_count(), 1);
        assert_eq!(aggregation.pending_recognitions, 1);
    }

    #[test]
    fn playback_reservations_are_bounded_and_keep_call_order_when_tts_finishes_out_of_order() {
        assert_eq!(MAX_QUEUED_PLAYBACKS, 20);
        let mut playback = VecDeque::new();
        let first_cancellation = tokio_util::sync::CancellationToken::new();
        let second_cancellation = tokio_util::sync::CancellationToken::new();
        assert!(reserve_playback(
            &mut playback,
            "first".to_owned(),
            first_cancellation,
        ));
        assert!(reserve_playback(
            &mut playback,
            "second".to_owned(),
            second_cancellation,
        ));

        let output = |id: &str| super::super::gateway::PlaybackOutput {
            output_id: id.to_owned(),
            run_id: assistant_protocol::RunId::new("run-playback-order").expect("static Run id"),
            text: id.to_owned(),
            pcm: Arc::from([0_u8, 0_u8]),
        };
        assert!(attach_playback_output(&mut playback, output("second")));
        assert!(playback.front().is_some_and(|entry| entry.output.is_none()));
        assert!(attach_playback_output(&mut playback, output("first")));
        assert_eq!(
            playback
                .iter()
                .map(|entry| entry.output_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        while playback.len() < MAX_QUEUED_PLAYBACKS {
            let index = playback.len();
            assert!(reserve_playback(
                &mut playback,
                format!("queued-{index}"),
                tokio_util::sync::CancellationToken::new(),
            ));
        }
        assert!(!reserve_playback(
            &mut playback,
            "overflow".to_owned(),
            tokio_util::sync::CancellationToken::new(),
        ));
    }

    #[test]
    fn ready_playback_can_start_immediately() {
        let mut playback = VecDeque::new();
        assert!(reserve_playback(
            &mut playback,
            "deferred".to_owned(),
            tokio_util::sync::CancellationToken::new(),
        ));
        assert!(attach_playback_output(
            &mut playback,
            super::super::gateway::PlaybackOutput {
                output_id: "deferred".to_owned(),
                run_id: assistant_protocol::RunId::new("run-deferred-playback")
                    .expect("static Run id"),
                text: "deferred".to_owned(),
                pcm: Arc::from([0_u8, 0_u8]),
            },
        ));

        assert!(playback_should_start(&playback));
    }

    #[test]
    fn playback_ack_rejects_cancelled_missing_duplicate_and_abandoned_output() {
        let mut playback = VecDeque::new();
        let token = tokio_util::sync::CancellationToken::new();
        assert!(reserve_playback(
            &mut playback,
            "output".to_owned(),
            token.clone()
        ));
        let output = || super::super::gateway::PlaybackOutput {
            output_id: "output".to_owned(),
            run_id: assistant_protocol::RunId::new("run").unwrap(),
            text: "播报".to_owned(),
            pcm: Arc::from([0_u8; 640]),
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        drop(rx);
        acknowledge_playback_output(&mut playback, output(), tx);
        assert!(playback.front().unwrap().output.is_none());
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        acknowledge_playback_output(&mut playback, output(), tx);
        assert_eq!(rx.try_recv(), Ok(true));
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        acknowledge_playback_output(&mut playback, output(), tx);
        assert_eq!(rx.try_recv(), Ok(false));
        token.cancel();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        acknowledge_playback_output(&mut playback, output(), tx);
        assert_eq!(rx.try_recv(), Ok(false));
        playback.clear();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        acknowledge_playback_output(&mut playback, output(), tx);
        assert_eq!(rx.try_recv(), Ok(false));
        assert!(playback.is_empty());
    }

    mod wss_integration {
        use std::net::SocketAddr;

        use assistant_protocol::{
            ConfirmDevicePairingRequest, ConnectDiscoveredDeviceRequest, DeviceId,
            DeviceLifecycleSnapshot, DevicePairingModeSnapshot, DeviceSpeechServicesSnapshot,
            GetMcpConfigurationRequest, McpServerRuntimeState, McpServerSource, McpTransportKind,
            RejectDevicePairingRequest, SecretValue, ShutdownRuntimeRequest, SpeechProviderKind,
            SpeechServiceStatusSnapshot, UpdateAsrConfigurationInput,
            UpdateSpeechConfigurationRequest, UpdateTtsConfigurationInput,
        };
        use assistant_runtime::{AssistantRuntime, RuntimeStore as _};
        use axum::{
            Router,
            body::Body,
            extract::Json as AxumJson,
            http::Response as HttpResponse,
            routing::{get, post},
        };
        use axum_server::tls_rustls::RustlsConfig;
        use ed25519_dalek::{Signer as _, SigningKey};
        use futures_util::SinkExt as _;
        use hmac::{Hmac, Mac};
        use pakery_core::crypto::CpaceGroup as _;
        use pakery_crypto::{P256Group, Spake2P256};
        use pakery_spake2::PartyA;
        use rustls_pki_types::{CertificateDer, pem::PemObject as _};
        use serde::de::DeserializeOwned;
        use serde_json::{Value, json};
        use sha2::{Sha256, Sha512};
        use tokio_tungstenite::{
            Connector, MaybeTlsStream, WebSocketStream, client_async_tls_with_config,
            tungstenite::Message as WsMessage,
        };

        use super::*;
        use crate::{
            config_source::{LocalConfigSource, prepare_private_directory},
            device::{
                dispatcher::DeviceChannelOutputDispatcher,
                gateway::{DeviceGatewayHandle, DeviceGatewayService},
                identity::InstallationIdentity,
                protocol::{
                    AuthChallenge, DeviceConsoleDeclaration, DeviceHello, DeviceMcpScheme,
                    DeviceMcpServerDeclaration, Envelope, HelloAck, PCM_FRAME_KIND_RELAY_DOWNLINK,
                    PCM_FRAME_KIND_RELAY_UPLINK, PCM_HEADER_BYTES, PROTOCOL_MAJOR, PROTOCOL_MINOR,
                    PairingBind, PairingBindAck, PairingCommit, PairingComplete,
                    PairingConfirmation, PairingDirectChallenge, PairingDirectCommit, PairingHello,
                    PairingPake, PairingPending, PcmFormat, SpeechAsrResult, SpeechAsrStart,
                    SpeechAsrStop, SpeechStatusChanged, SpeechTtsEnd, SpeechTtsRequest,
                    SpeechTtsStart, WireError, WirePairingMode,
                },
            },
            mcp::{HostMcpConnectionFactory, HostMcpImageMaterializer, LocalMcpConfigSource},
            resources::HostResources,
            speech::{SpeechService, SpeechServiceHandle},
            storage::LocalRuntimeStore,
        };

        type TestWs = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

        struct TestGatewayHarness {
            runtime: Arc<AssistantRuntime>,
            handle: DeviceGatewayHandle,
            speech: SpeechServiceHandle,
            identity: Arc<InstallationIdentity>,
            addr: SocketAddr,
            server_handle: axum_server::Handle<SocketAddr>,
            server_task: tokio::task::JoinHandle<std::io::Result<()>>,
            speech_cancel: tokio_util::sync::CancellationToken,
            speech_task: tokio::task::JoinHandle<Result<(), SpeechServiceError>>,
            mcp_factory: Arc<HostMcpConnectionFactory>,
            store: Arc<LocalRuntimeStore>,
            shared: Arc<GatewayShared>,
            _gateway_service: DeviceGatewayService,
            _directory: tempfile::TempDir,
        }

        impl TestGatewayHarness {
            async fn start() -> Self {
                let directory = tempfile::tempdir().expect("tempdir");
                prepare_private_directory(directory.path()).expect("prepare home dir");
                crate::server::initialize_tls_provider();
                let store = Arc::new(
                    LocalRuntimeStore::open(directory.path(), 32)
                        .await
                        .expect("open store"),
                );
                let resources = HostResources::new(directory.path()).expect("host resources");
                let mcp_source = Arc::new(LocalMcpConfigSource::new(directory.path().to_owned()));
                let mcp_factory =
                    Arc::new(HostMcpConnectionFactory::new(directory.path().to_owned()));
                let dispatcher = Arc::new(DeviceChannelOutputDispatcher::new());
                let runtime = Arc::new(
                    AssistantRuntime::open_with_recall_key(
                        assistant_runtime::RuntimeConfig::new(
                            std::num::NonZeroUsize::new(32).expect("nonzero"),
                        ),
                        Arc::new(LocalConfigSource::new(directory.path().join("config.toml"))),
                        resources.model_factory,
                        resources.session_environment_factory,
                        resources.skill_package_source,
                        resources.run_tool_factory,
                        resources.child_task_workspace_factory,
                        store.clone(),
                        store.clone(),
                        [42_u8; 32],
                    )
                    .await
                    .expect("open runtime")
                    .with_mcp_services(
                        mcp_source,
                        mcp_factory.clone(),
                        Arc::new(HostMcpImageMaterializer),
                    )
                    .with_channel_output_dispatcher(dispatcher.clone()),
                );
                let config_source =
                    Arc::new(LocalConfigSource::new(directory.path().join("config.toml")));
                let (speech_service, speech) = SpeechService::new(config_source);
                let speech_cancel = tokio_util::sync::CancellationToken::new();
                let speech_task = tokio::spawn(speech_service.run_until(speech_cancel.clone()));
                let (gateway_service, handle) = DeviceGatewayService::new(
                    directory.path().to_path_buf(),
                    runtime.clone(),
                    dispatcher.as_ref(),
                    speech.clone(),
                );
                let identity = Arc::new(
                    InstallationIdentity::load_or_create(directory.path()).expect("identity"),
                );
                let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind WSS");
                listener.set_nonblocking(true).expect("nonblocking WSS");
                let addr = listener.local_addr().expect("WSS addr");
                handle.seed_test_identity(identity.clone(), addr.port()).await;
                let tls = RustlsConfig::from_pem(
                    identity.certificate_pem.clone(),
                    identity.private_key_pem.clone(),
                )
                .await
                .expect("rustls config");
                let router = Router::new()
                    .route("/device", get(upgrade))
                    .with_state(handle.test_shared());
                let server_handle = axum_server::Handle::new();
                let server_handle_clone = server_handle.clone();
                let server_task = tokio::spawn(async move {
                    axum_server::from_tcp_rustls(listener, tls)
                        .expect("rustls server")
                        .handle(server_handle_clone)
                        .serve(router.into_make_service_with_connect_info::<SocketAddr>())
                        .await
                });

                let shared = handle.test_shared();
                Self {
                    runtime,
                    handle,
                    speech,
                    identity,
                    addr,
                    server_handle,
                    server_task,
                    speech_cancel,
                    speech_task,
                    mcp_factory,
                    store,
                    shared,
                    _gateway_service: gateway_service,
                    _directory: directory,
                }
            }

            async fn connect_ws(&self) -> (TestWs, AuthChallenge) {
                let mut roots = rustls::RootCertStore::empty();
                for cert in CertificateDer::pem_slice_iter(&self.identity.certificate_pem) {
                    roots.add(cert.expect("cert")).expect("add root");
                }
                let tls_config = rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                let stream = tokio::net::TcpStream::connect(self.addr)
                    .await
                    .expect("tcp connect");
                let url = format!("wss://localhost:{}/device", self.addr.port());
                let (mut ws, _) = client_async_tls_with_config(
                    url,
                    stream,
                    None,
                    Some(Connector::Rustls(Arc::new(tls_config))),
                )
                .await
                .expect("WSS handshake");
                let env = recv_envelope(&mut ws).await.expect("auth_challenge");
                assert_eq!(env.message_type, "auth_challenge");
                let challenge: AuthChallenge = decode_payload(env);
                (ws, challenge)
            }

            async fn shutdown(self) {
                self.server_handle.shutdown();
                let _ = self.server_task.await;
                self.speech_cancel.cancel();
                let _ = self.speech_task.await;
                let _ = self.runtime.shutdown(ShutdownRuntimeRequest {}).await;
                let _ = self.mcp_factory.shutdown().await;
                let _ = self.store.shutdown().await;
            }
        }

        fn text_capabilities() -> DeviceCapabilitiesSnapshot {
            DeviceCapabilitiesSnapshot {
                input_text: true,
                output_text: true,
                display_status: true,
                display_transcript: true,
                ..DeviceCapabilitiesSnapshot::default()
            }
        }

        async fn send_json<T: serde::Serialize>(
            ws: &mut TestWs,
            message_id: &str,
            message_type: &str,
            payload: &T,
        ) {
            let envelope = Envelope {
                protocol_major: PROTOCOL_MAJOR,
                protocol_minor: PROTOCOL_MINOR,
                message_id: message_id.to_owned(),
                message_type: message_type.to_owned(),
                payload: serde_json::to_value(payload).expect("serialize payload"),
            };
            ws.send(WsMessage::Text(
                serde_json::to_string(&envelope)
                    .expect("encode envelope")
                    .into(),
            ))
            .await
            .expect("send ws text");
        }

        async fn recv_envelope(ws: &mut TestWs) -> Option<Envelope> {
            loop {
                let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .expect("recv timeout")?;
                match message.expect("ws frame") {
                    WsMessage::Text(text) => {
                        return Some(Envelope::decode(&text).expect("decode envelope"));
                    }
                    WsMessage::Ping(payload) => {
                        ws.send(WsMessage::Pong(payload)).await.expect("send pong");
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(_) => return None,
                    other => panic!("unexpected ws message: {other:?}"),
                }
            }
        }

        fn decode_payload<T: DeserializeOwned>(envelope: Envelope) -> T {
            serde_json::from_value(envelope.payload).expect("decode payload")
        }

        fn spake2_password_scalar(
            pairing_code: &str,
            pairing_request_id: &str,
            installation_id: &str,
        ) -> <P256Group as pakery_core::crypto::CpaceGroup>::Scalar {
            let mut input = Vec::new();
            for part in [
                b"ez-assistant-spake2-password-v1".as_slice(),
                pairing_code.as_bytes(),
                pairing_request_id.as_bytes(),
                installation_id.as_bytes(),
            ] {
                input.extend_from_slice(&(part.len() as u64).to_be_bytes());
                input.extend_from_slice(part);
            }
            let wide = Sha512::digest(input);
            P256Group::scalar_from_wide_bytes(&wide).expect("scalar")
        }

        fn hmac_sha256_tagged(session_key: &[u8], label: &[u8], transcript: &[u8]) -> Vec<u8> {
            let mut mac = <Hmac<Sha256>>::new_from_slice(session_key).expect("hmac");
            mac.update(label);
            mac.update(transcript);
            mac.finalize().into_bytes().to_vec()
        }

        async fn pair_direct_confirm(
            harness: &TestGatewayHarness,
            request_id: &str,
            signing_key: &SigningKey,
            tamper_signature: bool,
        ) -> (TestWs, Option<DeviceId>) {
            harness
                .handle
                .open_pairing_window()
                .await
                .expect("open pairing window");
            let (mut ws, _) = harness.connect_ws().await;
            let public_key = signing_key.verifying_key().to_bytes();
            let device_nonce = "dev-direct-nonce-1".to_owned();
            send_json(
                &mut ws,
                "m-pair-hello",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: request_id.to_owned(),
                    display_name: "Direct Gateway".to_owned(),
                    device_nonce: device_nonce.clone(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(&public_key)),
                    client_version: Some("esp-pika-gateway/0.3.0".to_owned()),
                },
            )
            .await;

            let pending_env = recv_envelope(&mut ws).await.expect("pairing_pending");
            assert_eq!(pending_env.message_type, "pairing_pending");
            let _pending: PairingPending = decode_payload(pending_env);

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            assert_eq!(snapshot.pending_pairings.len(), 1);
            let candidate = &snapshot.pending_pairings[0];
            assert_eq!(candidate.mode, DevicePairingModeSnapshot::DirectConfirm);
            assert_eq!(candidate.peer_address.as_deref(), Some("127.0.0.1"));
            assert_eq!(
                candidate.key_fingerprint.as_deref(),
                Some(public_key_fingerprint(&public_key).as_str())
            );
            assert_eq!(
                candidate.client_version.as_deref(),
                Some("esp-pika-gateway/0.3.0")
            );

            harness
                .handle
                .confirm_pairing(ConfirmDevicePairingRequest {
                    pairing_request_id: request_id.to_owned(),
                    pairing_code: None,
                    display_name: None,
                })
                .await
                .expect("confirm direct pairing");

            let challenge_env = recv_envelope(&mut ws)
                .await
                .expect("pairing_direct_challenge");
            assert_eq!(challenge_env.message_type, "pairing_direct_challenge");
            let challenge: PairingDirectChallenge = decode_payload(challenge_env);
            assert_eq!(challenge.pairing_request_id, request_id);

            let ad = pairing_associated_data(
                request_id,
                &harness.identity.installation_id,
                &harness.identity.certificate_fingerprint,
                &device_nonce,
                &challenge.host_nonce,
                text_capabilities(),
            );
            let bind_transcript = pairing_bind_transcript(&ad, &public_key);
            let commit_transcript =
                pairing_commit_transcript(&bind_transcript, &challenge.device_id);
            let mut sig_bytes = signing_key.sign(&commit_transcript).to_bytes();
            if tamper_signature {
                sig_bytes[0] ^= 0xff;
            }
            send_json(
                &mut ws,
                "m-direct-commit",
                "pairing_direct_commit",
                &PairingDirectCommit {
                    pairing_request_id: request_id.to_owned(),
                    device_id: challenge.device_id.clone(),
                    signature: encode_base64(&sig_bytes),
                },
            )
            .await;

            let outcome = recv_envelope(&mut ws).await.expect("pairing outcome");
            if tamper_signature {
                assert_eq!(outcome.message_type, "error");
                let err: WireError = decode_payload(outcome);
                assert_eq!(err.code, "pairing_failed");
                assert!(!err.recoverable);
                (ws, None)
            } else {
                assert_eq!(outcome.message_type, "pairing_complete");
                let complete: PairingComplete = decode_payload(outcome);
                let device_id = DeviceId::new(complete.device_id).expect("device id");
                (ws, Some(device_id))
            }
        }

        #[tokio::test]
        async fn wss_direct_confirm_pairing_authenticates_and_persists_device() {
            let harness = TestGatewayHarness::start().await;
            let signing_key = SigningKey::from_bytes(&[11_u8; 32]);
            let (mut ws, device_id) =
                pair_direct_confirm(&harness, "req-direct-ok", &signing_key, false).await;
            let device_id = device_id.expect("paired device id");
            let _ = ws.close(None).await;

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            assert!(snapshot.pending_pairings.is_empty());
            assert_eq!(snapshot.devices.len(), 1);
            assert_eq!(snapshot.devices[0].device_id, device_id);
            assert_eq!(
                snapshot.devices[0].lifecycle,
                DeviceLifecycleSnapshot::Paired
            );

            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_direct_confirm_rejects_bad_signature_and_clears_pending() {
            let harness = TestGatewayHarness::start().await;
            let signing_key = SigningKey::from_bytes(&[12_u8; 32]);
            let (mut ws, device_id) =
                pair_direct_confirm(&harness, "req-direct-bad", &signing_key, true).await;
            assert!(device_id.is_none());
            let _ = ws.close(None).await;

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            assert!(snapshot.pending_pairings.is_empty());
            assert!(snapshot.devices.is_empty());

            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_reject_pairing_notifies_client_and_clears_pending() {
            let harness = TestGatewayHarness::start().await;
            harness
                .handle
                .open_pairing_window()
                .await
                .expect("open pairing window");
            let (mut ws, _) = harness.connect_ws().await;
            let signing_key = SigningKey::from_bytes(&[13_u8; 32]);
            send_json(
                &mut ws,
                "m-pair-reject",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: "req-reject".to_owned(),
                    display_name: "Rejected Device".to_owned(),
                    device_nonce: "nonce-reject".to_owned(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(signing_key.verifying_key().as_bytes())),
                    client_version: None,
                },
            )
            .await;

            assert_eq!(
                recv_envelope(&mut ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            harness
                .handle
                .reject_pairing(RejectDevicePairingRequest {
                    pairing_request_id: "req-reject".to_owned(),
                })
                .await
                .expect("reject pairing");

            let env = recv_envelope(&mut ws).await.expect("error frame");
            assert_eq!(env.message_type, "error");
            let wire_err: WireError = decode_payload(env);
            assert_eq!(wire_err.code, "pairing_rejected");
            assert!(!wire_err.recoverable);
            let _ = ws.close(None).await;

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            assert!(snapshot.pending_pairings.is_empty());
            assert!(snapshot.devices.is_empty());

            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_pairing_client_disconnect_cleans_up_pending_candidate() {
            let harness = TestGatewayHarness::start().await;
            harness
                .handle
                .open_pairing_window()
                .await
                .expect("open pairing window");
            let (mut ws, _) = harness.connect_ws().await;
            let signing_key = SigningKey::from_bytes(&[14_u8; 32]);
            send_json(
                &mut ws,
                "m-pair-drop",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: "req-disconnect".to_owned(),
                    display_name: "Dropping Device".to_owned(),
                    device_nonce: "nonce-drop".to_owned(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(signing_key.verifying_key().as_bytes())),
                    client_version: None,
                },
            )
            .await;
            assert_eq!(
                recv_envelope(&mut ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            assert_eq!(
                harness
                    .handle
                    .snapshot()
                    .await
                    .expect("snapshot")
                    .pending_pairings
                    .len(),
                1
            );

            drop(ws);
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if harness
                        .handle
                        .snapshot()
                        .await
                        .expect("snapshot")
                        .pending_pairings
                        .is_empty()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("pending candidate should clean up after client drop");

            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_legacy_spake2_pairing_still_succeeds() {
            let harness = TestGatewayHarness::start().await;
            harness
                .handle
                .open_pairing_window()
                .await
                .expect("open pairing window");
            let (mut ws, _) = harness.connect_ws().await;
            let pairing_code = "482913";
            let req_id = "req-legacy-spake2";
            let device_nonce = "dev-spake-nonce";
            let mut wide = [0_u8; 64];
            wide[32..].copy_from_slice(&[7_u8; 32]);
            let client_scalar = P256Group::scalar_from_wide_bytes(&wide).expect("scalar");
            let pw =
                spake2_password_scalar(pairing_code, req_id, &harness.identity.installation_id);
            let (dev_share, _) = PartyA::<Spake2P256>::start_with_scalar(
                &pw,
                &client_scalar,
                req_id.as_bytes(),
                harness.identity.installation_id.as_bytes(),
                b"",
            )
            .expect("spake2 party A");

            let hello_json = json!({
                "protocol_major": PROTOCOL_MAJOR,
                "protocol_minor": PROTOCOL_MINOR,
                "message_id": "m-legacy-hello",
                "type": "pairing_hello",
                "payload": {
                    "pairing_request_id": req_id,
                    "display_name": "Legacy Device",
                    "device_nonce": device_nonce,
                    "capabilities": text_capabilities(),
                    "pake_share": encode_base64(&dev_share),
                }
            });
            ws.send(WsMessage::Text(hello_json.to_string().into()))
                .await
                .expect("send legacy pairing_hello");

            assert_eq!(
                recv_envelope(&mut ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            assert_eq!(snapshot.pending_pairings.len(), 1);
            assert_eq!(
                snapshot.pending_pairings[0].mode,
                DevicePairingModeSnapshot::Spake2
            );

            harness
                .handle
                .confirm_pairing(ConfirmDevicePairingRequest {
                    pairing_request_id: req_id.to_owned(),
                    pairing_code: Some(SecretValue::new(pairing_code.to_owned())),
                    display_name: Some("Legacy Renamed".to_owned()),
                })
                .await
                .expect("confirm spake2");

            let pake_env = recv_envelope(&mut ws).await.expect("pairing_pake");
            assert_eq!(pake_env.message_type, "pairing_pake");
            let pake: PairingPake = decode_payload(pake_env);
            let host_share =
                decode_base64(&pake.pake_share, SPAKE2_P256_SHARE_BYTES).expect("host share");
            let host_conf =
                decode_base64(&pake.confirmation_mac, HMAC_SHA256_BYTES).expect("host conf");

            let ad = pairing_associated_data(
                req_id,
                &harness.identity.installation_id,
                &harness.identity.certificate_fingerprint,
                device_nonce,
                &pake.host_nonce,
                text_capabilities(),
            );
            let (_, state_a) = PartyA::<Spake2P256>::start_with_scalar(
                &pw,
                &client_scalar,
                req_id.as_bytes(),
                harness.identity.installation_id.as_bytes(),
                &ad,
            )
            .expect("spake2 party A with ad");
            let output = state_a.finish(&host_share).expect("spake2 finish");
            output
                .verify_peer_confirmation(&host_conf)
                .expect("host confirmation matches");

            send_json(
                &mut ws,
                "m-confirm",
                "pairing_confirm",
                &PairingConfirmation {
                    pairing_request_id: req_id.to_owned(),
                    confirmation_mac: encode_base64(&output.confirmation_mac),
                },
            )
            .await;

            let signing_key = SigningKey::from_bytes(&[15_u8; 32]);
            let pub_bytes = signing_key.verifying_key().to_bytes();
            let bt = pairing_bind_transcript(&ad, &pub_bytes);
            let bind_sig = signing_key.sign(&bt).to_bytes();
            let bind_mac = hmac_sha256_tagged(output.session_key.as_bytes(), b"device-bind", &bt);
            send_json(
                &mut ws,
                "m-bind",
                "pairing_bind",
                &PairingBind {
                    pairing_request_id: req_id.to_owned(),
                    public_key: encode_base64(&pub_bytes),
                    signature: encode_base64(&bind_sig),
                    binding_mac: encode_base64(&bind_mac),
                },
            )
            .await;

            let bind_ack_env = recv_envelope(&mut ws).await.expect("pairing_bind_ack");
            assert_eq!(bind_ack_env.message_type, "pairing_bind_ack");
            let bind_ack: PairingBindAck = decode_payload(bind_ack_env);
            let ct = pairing_commit_transcript(&bt, &bind_ack.device_id);
            let commit_sig = signing_key.sign(&ct).to_bytes();
            let commit_mac =
                hmac_sha256_tagged(output.session_key.as_bytes(), b"device-commit", &ct);
            send_json(
                &mut ws,
                "m-commit",
                "pairing_commit",
                &PairingCommit {
                    pairing_request_id: req_id.to_owned(),
                    device_id: bind_ack.device_id.clone(),
                    signature: encode_base64(&commit_sig),
                    binding_mac: encode_base64(&commit_mac),
                },
            )
            .await;

            let complete_env = recv_envelope(&mut ws).await.expect("pairing_complete");
            assert_eq!(complete_env.message_type, "pairing_complete");
            let complete: PairingComplete = decode_payload(complete_env);
            assert_eq!(complete.display_name, "Legacy Renamed");
            let _ = ws.close(None).await;

            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_authenticated_device_hello_syncs_marks_offline_and_removes_device_mcp() {
            let observed_headers =
                Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
            let observed_headers_handler = observed_headers.clone();
            let mcp_router = Router::new().route(
                "/mcp",
                post(
                    move |headers: axum::http::HeaderMap, AxumJson(request): AxumJson<Value>| {
                        let observed_headers = observed_headers_handler.clone();
                        async move {
                            let agent_id = headers
                                .get("x-agent-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_owned();
                            let speaker_role = headers
                                .get("x-speaker-role")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default()
                                .to_owned();
                            if !agent_id.is_empty() || !speaker_role.is_empty() {
                                observed_headers.lock().await.push((agent_id, speaker_role));
                            }
                            let method = request["method"].as_str().unwrap_or_default();
                            let body = match method {
                                "server/discover" => json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "error": {"code": -32601, "message": "not supported"}
                                }),
                                "initialize" => json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": {
                                        "protocolVersion": "2025-06-18",
                                        "capabilities": {"tools": {}},
                                        "serverInfo": {"name": "mock-gateway-mcp", "version": "0.1.0"}
                                    }
                                }),
                                "notifications/initialized" => {
                                    return HttpResponse::builder()
                                        .status(202)
                                        .body(Body::empty())
                                        .expect("202");
                                }
                                "tools/list" => json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "result": {
                                        "tools": [{
                                            "name": "device.fleet_list",
                                            "description": "List online devices",
                                            "inputSchema": {"type": "object"}
                                        }]
                                    }
                                }),
                                other => json!({
                                    "jsonrpc": "2.0",
                                    "id": request["id"],
                                    "error": {"code": -32601, "message": format!("unknown method {other}")}
                                }),
                            };
                            HttpResponse::builder()
                                .header("content-type", "application/json")
                                .body(Body::from(body.to_string()))
                                .expect("json response")
                        }
                    },
                ),
            );
            let mcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock mcp");
            let mcp_port = mcp_listener.local_addr().expect("mcp addr").port();
            let mcp_server =
                tokio::spawn(async move { axum::serve(mcp_listener, mcp_router).await });

            let harness = TestGatewayHarness::start().await;
            let signing_key = SigningKey::from_bytes(&[16_u8; 32]);
            let (mut pair_ws, paired_id) =
                pair_direct_confirm(&harness, "req-mcp-sync", &signing_key, false).await;
            let device_id = paired_id.expect("paired id");
            let _ = pair_ws.close(None).await;

            let (mut ws, challenge) = harness.connect_ws().await;
            let device_nonce = "dev-auth-nonce-mcp";
            let transcript = auth_transcript(
                &challenge.connection_id,
                &challenge.nonce,
                device_id.as_str(),
                device_nonce,
                text_capabilities(),
                OutputPreferenceSnapshot::Text,
            );
            let sig = signing_key.sign(&transcript).to_bytes();
            send_json(
                &mut ws,
                "m-hello-mcp",
                "hello",
                &DeviceHello {
                    device_id: device_id.to_string(),
                    device_nonce: device_nonce.to_owned(),
                    capabilities: text_capabilities(),
                    output_preference: OutputPreferenceSnapshot::Text,
                    client_version: "esp-pika-gateway/0.3.0".to_owned(),
                    signature: encode_base64(&sig),
                    mcp_servers: vec![DeviceMcpServerDeclaration {
                        id: Some("esp-pika-gateway".to_owned()),
                        display_name: "ESP-Pika 设备网关".to_owned(),
                        description: "控制局域网终端".to_owned(),
                        scheme: DeviceMcpScheme::Http,
                        port: mcp_port,
                        path: "/mcp".to_owned(),
                        headers: std::collections::BTreeMap::from([
                            ("X-Agent-ID".to_owned(), "ez-assistant".to_owned()),
                            ("X-Speaker-Role".to_owned(), "owner".to_owned()),
                        ]),
                    }],
                    console: None,
                    speech_relay: false,
                    hardware: None,
                },
            )
            .await;

            let ack_env = recv_envelope(&mut ws).await.expect("hello_ack");
            assert_eq!(ack_env.message_type, "hello_ack");
            let _ack: HelloAck = decode_payload(ack_env);

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            let conn = snapshot.devices[0]
                .connection
                .as_ref()
                .expect("online connection");
            assert_eq!(conn.peer_address.as_deref(), Some("127.0.0.1"));
            assert_eq!(
                conn.client_version.as_deref(),
                Some("esp-pika-gateway/0.3.0")
            );
            assert_eq!(conn.mcp_server_keys.len(), 1);
            let expected_key = conn.mcp_server_keys[0].clone();

            let online_cfg = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let cfg = harness
                        .runtime
                        .get_mcp_configuration(GetMcpConfigurationRequest {})
                        .await
                        .expect("get mcp config");
                    if let Some(server) = cfg
                        .snapshot
                        .servers
                        .iter()
                        .find(|server| server.server_key == expected_key)
                        && server.tool_count == 1
                    {
                        break cfg.snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("device MCP server synced");
            let synced_server = online_cfg
                .servers
                .iter()
                .find(|server| server.server_key == expected_key)
                .expect("device mcp entry");
            assert_eq!(
                synced_server.source,
                McpServerSource::Device(device_id.to_string())
            );
            assert!(
                synced_server
                    .managed
                    .as_ref()
                    .expect("managed")
                    .source_enabled
            );
            assert_eq!(synced_server.transport, McpTransportKind::StreamableHttp);
            assert_eq!(
                synced_server.runtime_state,
                McpServerRuntimeState::Connected
            );
            assert_eq!(synced_server.target_summary, "Managed HTTP");
            assert!(
                observed_headers
                    .lock()
                    .await
                    .contains(&("ez-assistant".to_owned(), "owner".to_owned()))
            );

            drop(ws);
            let offline_cfg = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let cfg = harness
                        .runtime
                        .get_mcp_configuration(GetMcpConfigurationRequest {})
                        .await
                        .expect("get mcp config");
                    if let Some(server) = cfg
                        .snapshot
                        .servers
                        .iter()
                        .find(|server| server.server_key == expected_key)
                        && server
                            .managed
                            .as_ref()
                            .is_some_and(|managed| !managed.source_enabled)
                    {
                        break cfg.snapshot;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("device MCP marked offline after disconnect");
            assert!(
                !offline_cfg
                    .servers
                    .iter()
                    .find(|server| server.server_key == expected_key)
                    .expect("server still present while offline")
                    .managed
                    .as_ref()
                    .expect("managed")
                    .source_enabled
            );

            harness
                .runtime
                .revoke_paired_device(device_id.clone())
                .await
                .expect("revoke paired device");
            harness.handle.revoke_connection(&device_id).await;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let cfg = harness
                        .runtime
                        .get_mcp_configuration(GetMcpConfigurationRequest {})
                        .await
                        .expect("get mcp config");
                    if !cfg
                        .snapshot
                        .servers
                        .iter()
                        .any(|server| server.server_key == expected_key)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(30)).await;
                }
            })
            .await
            .expect("device MCP removed on revoke");

            mcp_server.abort();
            let _ = mcp_server.await;
            harness.shutdown().await;
        }

        fn build_test_wav_pcm16_16k_mono(samples: &[i16]) -> Vec<u8> {
            let data_bytes = (samples.len() * 2) as u32;
            let mut wav = Vec::with_capacity(44 + data_bytes as usize);
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
            wav.extend_from_slice(b"WAVE");
            wav.extend_from_slice(b"fmt ");
            wav.extend_from_slice(&16_u32.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&16_000_u32.to_le_bytes());
            wav.extend_from_slice(&32_000_u32.to_le_bytes());
            wav.extend_from_slice(&2_u16.to_le_bytes());
            wav.extend_from_slice(&16_u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&data_bytes.to_le_bytes());
            for &sample in samples {
                wav.extend_from_slice(&sample.to_le_bytes());
            }
            wav
        }

        fn encode_relay_uplink_frame(stream_id: u32, sequence: u32, payload: &[u8]) -> Vec<u8> {
            let mut frame = vec![0_u8; PCM_HEADER_BYTES + payload.len()];
            frame[0] = 1;
            frame[1] = PCM_FRAME_KIND_RELAY_UPLINK;
            frame[4..8].copy_from_slice(&stream_id.to_be_bytes());
            frame[8..12].copy_from_slice(&sequence.to_be_bytes());
            frame[12..14].copy_from_slice(&(payload.len() as u16).to_be_bytes());
            frame[PCM_HEADER_BYTES..].copy_from_slice(payload);
            frame
        }

        #[tokio::test]
        async fn wss_authenticated_console_and_speech_relay_round_trip() {
            let tts_samples = [321_i16, -654, 987, -111];
            let tts_wav = build_test_wav_pcm16_16k_mono(&tts_samples);
            let speech_router = Router::new()
                .route(
                    "/v1/audio/transcriptions",
                    post(|| async { AxumJson(json!({ "text": "打开客厅灯" })) }),
                )
                .route(
                    "/v1/audio/speech",
                    post({
                        let tts_wav = tts_wav.clone();
                        move || {
                            let body = tts_wav.clone();
                            async move {
                                HttpResponse::builder()
                                    .header("content-type", "audio/wav")
                                    .body(Body::from(body))
                                    .expect("wav response")
                            }
                        }
                    }),
                );
            let speech_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind speech mock");
            let speech_port = speech_listener.local_addr().expect("speech addr").port();
            let speech_server = tokio::spawn(async move {
                axum::serve(speech_listener, speech_router)
                    .await
                    .expect("serve speech mock");
            });

            let harness = TestGatewayHarness::start().await;
            let initial_cfg = harness
                .speech
                .get_configuration()
                .await
                .expect("read initial config");
            let updated_cfg = harness
                .speech
                .update_configuration(UpdateSpeechConfigurationRequest {
                    expected_revision: initial_cfg.revision,
                    asr: UpdateAsrConfigurationInput {
                        enabled: true,
                        provider: SpeechProviderKind::Openai,
                        endpoint: format!("http://127.0.0.1:{speech_port}/v1"),
                        model: "gpt-4o-mini-transcribe".to_owned(),
                        timeout_ms: 10_000,
                        credential: Some(SecretValue::new("sk-asr-test".to_owned())),
                        clear_credential: false,
                    },
                    tts: UpdateTtsConfigurationInput {
                        enabled: true,
                        provider: SpeechProviderKind::Openai,
                        endpoint: format!("http://127.0.0.1:{speech_port}/v1"),
                        model: "gpt-4o-mini-tts".to_owned(),
                        voice: "alloy".to_owned(),
                        timeout_ms: 10_000,
                        credential: Some(SecretValue::new("sk-tts-test".to_owned())),
                        clear_credential: false,
                        reuse_asr_credential: false,
                    },
                })
                .await
                .expect("update speech config");
            assert_eq!(updated_cfg.status.asr, SpeechServiceStatusSnapshot::Ready);
            assert_eq!(updated_cfg.status.tts, SpeechServiceStatusSnapshot::Ready);

            let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
            let (mut ws, paired_id) =
                pair_direct_confirm(&harness, "pair-relay-1", &signing_key, false).await;
            let device_id = paired_id.expect("paired device id");
            let _ = ws.close(None).await;

            let (mut ws, challenge) = harness.connect_ws().await;
            let device_nonce = "dev-auth-relay-1";
            let caps = text_capabilities();
            let transcript = auth_transcript(
                &challenge.connection_id,
                &challenge.nonce,
                device_id.as_str(),
                device_nonce,
                caps,
                OutputPreferenceSnapshot::Text,
            );
            let sig = signing_key.sign(&transcript).to_bytes();
            send_json(
                &mut ws,
                "m-hello-relay",
                "hello",
                &DeviceHello {
                    device_id: device_id.to_string(),
                    device_nonce: device_nonce.to_owned(),
                    capabilities: caps,
                    output_preference: OutputPreferenceSnapshot::Text,
                    client_version: "esp-pika-gateway/0.4.0".to_owned(),
                    signature: encode_base64(&sig),
                    mcp_servers: Vec::new(),
                    console: Some(DeviceConsoleDeclaration {
                        scheme: DeviceMcpScheme::Http,
                        port: 8000,
                        api_prefix: "/api/v1".to_owned(),
                        ui_path: Some("/ui/".to_owned()),
                    }),
                    speech_relay: true,
                    hardware: None,
                },
            )
            .await;

            let ack_env = recv_envelope(&mut ws).await.expect("hello_ack");
            assert_eq!(ack_env.message_type, "hello_ack");
            let ack: HelloAck = decode_payload(ack_env);
            let ack_speech = ack.speech_services.expect("speech services in hello_ack");
            assert_eq!(ack_speech.asr, SpeechServiceStatusSnapshot::Ready);
            assert_eq!(ack_speech.tts, SpeechServiceStatusSnapshot::Ready);

            let snapshot = harness.handle.snapshot().await.expect("snapshot");
            let conn = snapshot.devices[0]
                .connection
                .as_ref()
                .expect("online connection");
            assert!(conn.speech_relay);
            let console_snap = conn.console.as_ref().expect("console snapshot");
            assert_eq!(console_snap.api_prefix, "/api/v1");
            assert_eq!(console_snap.ui_path.as_deref(), Some("/ui/"));
            assert_eq!(
                console_snap.direct_url.as_deref(),
                Some("http://127.0.0.1:8000/ui/")
            );
            assert!(harness.handle.console_target(&device_id).await.is_some());

            harness
                .shared
                .broadcast_speech_status(DeviceSpeechServicesSnapshot {
                    asr: SpeechServiceStatusSnapshot::Degraded,
                    tts: SpeechServiceStatusSnapshot::Ready,
                })
                .await;
            let status_env = recv_envelope(&mut ws).await.expect("speech_status_changed");
            assert_eq!(status_env.message_type, "speech_status_changed");
            let status_payload: SpeechStatusChanged = decode_payload(status_env);
            assert_eq!(
                status_payload.speech_services.asr,
                SpeechServiceStatusSnapshot::Degraded
            );

            send_json(
                &mut ws,
                "m-asr-start",
                "speech_asr_start",
                &SpeechAsrStart {
                    request_id: "asr-req-1".to_owned(),
                    stream_id: 101,
                    format: PcmFormat {
                        encoding: "pcm_s16le".to_owned(),
                        sample_rate_hz: 16_000,
                        channels: 1,
                        frame_duration_ms: 0,
                    },
                },
            )
            .await;
            let uplink_pcm = vec![12_u8; 640];
            ws.send(WsMessage::Binary(
                encode_relay_uplink_frame(101, 0, &uplink_pcm).into(),
            ))
            .await
            .expect("send binary kind=3");
            send_json(
                &mut ws,
                "m-asr-stop",
                "speech_asr_stop",
                &SpeechAsrStop {
                    request_id: "asr-req-1".to_owned(),
                    stream_id: 101,
                    last_sequence: 0,
                },
            )
            .await;
            let asr_env = recv_envelope(&mut ws).await.expect("speech_asr_result");
            assert_eq!(asr_env.message_type, "speech_asr_result");
            let asr_res: SpeechAsrResult = decode_payload(asr_env);
            assert!(asr_res.ok);
            assert_eq!(asr_res.text.as_deref(), Some("打开客厅灯"));

            send_json(
                &mut ws,
                "m-tts-req",
                "speech_tts_request",
                &SpeechTtsRequest {
                    request_id: "tts-req-1".to_owned(),
                    stream_id: 202,
                    text: "客厅灯已打开".to_owned(),
                },
            )
            .await;
            let tts_start_env = recv_envelope(&mut ws).await.expect("speech_tts_start");
            assert_eq!(tts_start_env.message_type, "speech_tts_start");
            let tts_start: SpeechTtsStart = decode_payload(tts_start_env);
            assert!(tts_start.ok);
            assert_eq!(tts_start.total_bytes, Some((tts_samples.len() * 2) as u64));

            let mut received_pcm = Vec::new();
            let tts_end_env = loop {
                let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await
                    .expect("tts recv")
                    .expect("tts frame")
                    .expect("ws ok");
                match msg {
                    WsMessage::Binary(frame) => {
                        assert_eq!(frame[0], 1);
                        assert_eq!(frame[1], PCM_FRAME_KIND_RELAY_DOWNLINK);
                        assert_eq!(
                            u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]),
                            202
                        );
                        received_pcm.extend_from_slice(&frame[PCM_HEADER_BYTES..]);
                    }
                    WsMessage::Text(text) => {
                        break Envelope::decode(&text).expect("decode tts end");
                    }
                    other => panic!("unexpected TTS ws frame: {other:?}"),
                }
            };
            assert_eq!(tts_end_env.message_type, "speech_tts_end");
            let tts_end: SpeechTtsEnd = decode_payload(tts_end_env);
            assert!(tts_end.ok);
            let mut expected_pcm = Vec::new();
            for s in tts_samples {
                expected_pcm.extend_from_slice(&s.to_le_bytes());
            }
            assert_eq!(received_pcm, expected_pcm);

            speech_server.abort();
            let _ = speech_server.await;
            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_connect_discovered_device_nudges_http_and_auto_approves_matching_key_only() {
            let harness = TestGatewayHarness::start().await;
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(4);
            let mock_device_app = Router::new().route(
                "/ez/connect",
                post({
                    let tx = tx.clone();
                    move |AxumJson(body): AxumJson<Value>| {
                        let tx = tx.clone();
                        async move {
                            let _ = tx.send(body).await;
                            axum::Json(json!({ "ok": true }))
                        }
                    }
                }),
            );
            let dev_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind mock device http");
            let dev_addr = dev_listener.local_addr().expect("mock device addr");
            let dev_server = tokio::spawn(async move {
                let _ = axum::serve(dev_listener, mock_device_app).await;
            });

            let target_key = SigningKey::from_bytes(&[71_u8; 32]);
            let target_pub = target_key.verifying_key().to_bytes();
            let target_fp = public_key_fingerprint(&target_pub);
            let discovery_id = "Mosaico-5BF0._ez-device._tcp.local.";
            harness
                .handle
                .seed_test_discovered_device(
                    discovery_id,
                    "Mosaico-5BF0",
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    dev_addr.port(),
                    &target_fp,
                )
                .await;

            let initial = harness.handle.snapshot().await.expect("initial snapshot");
            assert_eq!(initial.discovered_devices.len(), 1);
            assert_eq!(initial.discovered_devices[0].key_fingerprint, target_fp);

            harness
                .handle
                .connect_discovered_device(ConnectDiscoveredDeviceRequest {
                    discovery_id: discovery_id.to_owned(),
                    display_name: Some("书房 Mosaico".to_owned()),
                })
                .await
                .expect("connect discovered device");

            let nudge = tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .expect("nudge timeout")
                .expect("nudge body");
            assert_eq!(nudge["port"].as_u64(), Some(u64::from(harness.addr.port())));
            assert_eq!(nudge["path"].as_str(), Some("/device"));
            assert_eq!(
                nudge["installation_id"].as_str(),
                Some(harness.identity.installation_id.as_str())
            );
            assert_eq!(
                nudge["certificate_fingerprint"].as_str(),
                Some(harness.identity.certificate_fingerprint.as_str())
            );

            // A different key connecting while targeted slot is active must NOT auto-approve.
            let rogue_key = SigningKey::from_bytes(&[72_u8; 32]);
            let (mut rogue_ws, _) = harness.connect_ws().await;
            send_json(
                &mut rogue_ws,
                "m-rogue-pair",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: "req-rogue".to_owned(),
                    display_name: "Rogue".to_owned(),
                    device_nonce: "rogue-nonce".to_owned(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(rogue_key.verifying_key().as_bytes())),
                    client_version: None,
                },
            )
            .await;
            assert_eq!(
                recv_envelope(&mut rogue_ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            let rogue_snapshot = harness.handle.snapshot().await.expect("rogue snapshot");
            assert_eq!(rogue_snapshot.pending_pairings.len(), 1);
            let _ = rogue_ws.close(None).await;

            // Now the targeted Mosaico connects and auto-approves without manual confirm_pairing!
            let (mut ws, _) = harness.connect_ws().await;
            let req_id = "req-mosaico-auto";
            let dev_nonce = "dev-nonce-auto";
            send_json(
                &mut ws,
                "m-auto-pair",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: req_id.to_owned(),
                    display_name: "Mosaico-5BF0".to_owned(),
                    device_nonce: dev_nonce.to_owned(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(&target_pub)),
                    client_version: Some("esp-pika-mosaico/0.3.0".to_owned()),
                },
            )
            .await;
            assert_eq!(
                recv_envelope(&mut ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            let challenge_env = recv_envelope(&mut ws)
                .await
                .expect("auto pairing_direct_challenge");
            assert_eq!(challenge_env.message_type, "pairing_direct_challenge");
            let challenge: PairingDirectChallenge = decode_payload(challenge_env);
            let ad = pairing_associated_data(
                req_id,
                &harness.identity.installation_id,
                &harness.identity.certificate_fingerprint,
                dev_nonce,
                &challenge.host_nonce,
                text_capabilities(),
            );
            let bind_transcript = pairing_bind_transcript(&ad, &target_pub);
            let commit_transcript =
                pairing_commit_transcript(&bind_transcript, &challenge.device_id);
            let sig = target_key.sign(&commit_transcript).to_bytes();
            send_json(
                &mut ws,
                "m-auto-commit",
                "pairing_direct_commit",
                &PairingDirectCommit {
                    pairing_request_id: req_id.to_owned(),
                    device_id: challenge.device_id.clone(),
                    signature: encode_base64(&sig),
                },
            )
            .await;
            let complete_env = recv_envelope(&mut ws).await.expect("pairing_complete");
            assert_eq!(complete_env.message_type, "pairing_complete");
            let complete: PairingComplete = decode_payload(complete_env);
            assert_eq!(complete.display_name, "书房 Mosaico");
            let _ = ws.close(None).await;

            let after = harness.handle.snapshot().await.expect("after snapshot");
            assert!(after.discovered_devices.is_empty());
            assert_eq!(after.devices.len(), 1);
            assert_eq!(after.devices[0].display_name, "书房 Mosaico");

            dev_server.abort();
            let _ = dev_server.await;
            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_connect_discovered_device_approves_existing_pending_and_allows_repair_after_revoke() {
            let harness = TestGatewayHarness::start().await;
            let device_key = SigningKey::from_bytes(&[73_u8; 32]);
            let device_pub = device_key.verifying_key().to_bytes();
            let device_fp = public_key_fingerprint(&device_pub);
            let discovery_id = "Mosaico-Repair._ez-device._tcp.local.";

            // 1. 先登记并撤销该硬件密钥对应的旧设备身份（模拟用户曾点击「解除配对」）。
            let original_id = DeviceId::new("device-mosaico-revoked-01").expect("device id");
            harness
                .runtime
                .register_paired_device(NewPairedDevice {
                    device_id: original_id.clone(),
                    display_name: "Mosaico-5BF0".to_owned(),
                    public_key: DevicePublicKey::new(device_pub),
                    paired_at_ms: 1_000,
                })
                .await
                .expect("seed initial paired device");
            harness
                .runtime
                .revoke_paired_device(original_id.clone())
                .await
                .expect("revoke initial device");

            // 2. 打开配对窗口，终端先连入并停在 pending_pairings（与截图场景一致）。
            harness
                .handle
                .open_pairing_window()
                .await
                .expect("open pairing window");
            harness
                .handle
                .seed_test_discovered_device(
                    discovery_id,
                    "Mosaico-5BF0",
                    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    8765,
                    &device_fp,
                )
                .await;

            let (mut ws, _) = harness.connect_ws().await;
            let req_id = "req-mosaico-repair";
            let dev_nonce = "dev-nonce-repair";
            send_json(
                &mut ws,
                "m-repair-pair",
                "pairing_hello",
                &PairingHello {
                    pairing_request_id: req_id.to_owned(),
                    display_name: "Mosaico-5BF0".to_owned(),
                    device_nonce: dev_nonce.to_owned(),
                    capabilities: text_capabilities(),
                    pairing_mode: WirePairingMode::DirectConfirm,
                    pake_share: None,
                    public_key: Some(encode_base64(&device_pub)),
                    client_version: Some("esp-pika-mosaico/0.5.0-direct".to_owned()),
                },
            )
            .await;
            assert_eq!(
                recv_envelope(&mut ws).await.expect("pending").message_type,
                "pairing_pending"
            );
            let pending_snapshot = harness.handle.snapshot().await.expect("pending snapshot");
            assert_eq!(pending_snapshot.pending_pairings.len(), 1);

            // 3. 点击「连接」应直接批准现有 pending_pairings，并原地重新激活原 device_id！
            harness
                .handle
                .connect_discovered_device(ConnectDiscoveredDeviceRequest {
                    discovery_id: discovery_id.to_owned(),
                    display_name: None,
                })
                .await
                .expect("connect discovered approves existing pending");

            let challenge_env = recv_envelope(&mut ws)
                .await
                .expect("pairing_direct_challenge");
            assert_eq!(challenge_env.message_type, "pairing_direct_challenge");
            let challenge: PairingDirectChallenge = decode_payload(challenge_env);
            assert_eq!(challenge.device_id, original_id.as_str());

            let ad = pairing_associated_data(
                req_id,
                &harness.identity.installation_id,
                &harness.identity.certificate_fingerprint,
                dev_nonce,
                &challenge.host_nonce,
                text_capabilities(),
            );
            let bind_transcript = pairing_bind_transcript(&ad, &device_pub);
            let commit_transcript =
                pairing_commit_transcript(&bind_transcript, &challenge.device_id);
            let sig = device_key.sign(&commit_transcript).to_bytes();
            send_json(
                &mut ws,
                "m-repair-commit",
                "pairing_direct_commit",
                &PairingDirectCommit {
                    pairing_request_id: req_id.to_owned(),
                    device_id: challenge.device_id.clone(),
                    signature: encode_base64(&sig),
                },
            )
            .await;
            let complete_env = recv_envelope(&mut ws).await.expect("pairing_complete");
            assert_eq!(complete_env.message_type, "pairing_complete");
            let complete: PairingComplete = decode_payload(complete_env);
            assert_eq!(complete.device_id, original_id.as_str());
            let _ = ws.close(None).await;

            // 4. 快照中该设备已恢复 Paired，且能重新认证上线。
            let after = harness.handle.snapshot().await.expect("after snapshot");
            assert!(after.discovered_devices.is_empty());
            assert!(after.pending_pairings.is_empty());
            let device = after
                .devices
                .iter()
                .find(|item| item.device_id == original_id)
                .expect("reactivated device present");
            assert_eq!(
                device.lifecycle,
                assistant_protocol::DeviceLifecycleSnapshot::Paired
            );

            let (mut auth_ws, auth_challenge) = harness.connect_ws().await;
            let auth_nonce = "dev-auth-nonce-repair";
            let auth_t = auth_transcript(
                &auth_challenge.connection_id,
                &auth_challenge.nonce,
                original_id.as_str(),
                auth_nonce,
                text_capabilities(),
                OutputPreferenceSnapshot::Text,
            );
            let auth_sig = device_key.sign(&auth_t).to_bytes();
            send_json(
                &mut auth_ws,
                "m-repair-auth",
                "hello",
                &DeviceHello {
                    device_id: original_id.to_string(),
                    device_nonce: auth_nonce.to_owned(),
                    capabilities: text_capabilities(),
                    output_preference: OutputPreferenceSnapshot::Text,
                    client_version: "esp-pika-mosaico/0.5.0-direct".to_owned(),
                    signature: encode_base64(&auth_sig),
                    mcp_servers: Vec::new(),
                    console: None,
                    hardware: None,
                    speech_relay: false,
                },
            )
            .await;
            assert_eq!(
                recv_envelope(&mut auth_ws)
                    .await
                    .expect("hello_ack")
                    .message_type,
                "hello_ack"
            );
            let _ = auth_ws.close(None).await;
            harness.shutdown().await;
        }

        #[tokio::test]
        async fn wss_native_console_commands_acl_and_voiceprint_sync_round_trip() {
            let harness = TestGatewayHarness::start().await;
            let mcp_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind native mcp");
            let mcp_port = mcp_listener.local_addr().expect("mcp addr").port();
            let mcp_token = "test-loopback-mcp-token-0123456789".to_owned();
            let mcp_router = Router::new()
                .route(
                    "/mcp/{device_id}",
                    post(crate::device::native_console::handle_mcp_http_request),
                )
                .with_state(harness.shared.clone());
            let mcp_server = tokio::spawn(async move {
                let _ = axum::serve(mcp_listener, mcp_router).await;
            });
            harness
                .handle
                .seed_test_mcp_endpoint(mcp_port, mcp_token.clone())
                .await;

            let signing_key = SigningKey::from_bytes(&[73_u8; 32]);
            let public_key = signing_key.verifying_key().to_bytes();
            let device_id = DeviceId::new("dev-mosaico-direct").expect("device id");
            harness
                .runtime
                .register_paired_device(NewPairedDevice {
                    device_id: device_id.clone(),
                    display_name: "Mosaico-5BF0".to_owned(),
                    public_key: DevicePublicKey::new(public_key),
                    paired_at_ms: 1000,
                })
                .await
                .expect("register paired device");

            let (mut ws, challenge) = harness.connect_ws().await;
            let device_nonce = "dev-nonce-native-1";
            let caps = text_capabilities();
            let transcript = auth_transcript(
                &challenge.connection_id,
                &challenge.nonce,
                device_id.as_str(),
                device_nonce,
                caps,
                OutputPreferenceSnapshot::Text,
            );
            let sig = signing_key.sign(&transcript).to_bytes();
            send_json(
                &mut ws,
                "m-hello-native",
                "hello",
                &DeviceHello {
                    device_id: device_id.to_string(),
                    device_nonce: device_nonce.to_owned(),
                    capabilities: caps,
                    output_preference: OutputPreferenceSnapshot::Text,
                    client_version: "0.4.0-direct".to_owned(),
                    signature: encode_base64(&sig),
                    mcp_servers: Vec::new(),
                    console: None,
                    speech_relay: false,
                    hardware: Some(crate::device::protocol::DeviceHardwareDeclaration {
                        mac: "30:ed:a0:f4:5b:f0".to_owned(),
                        chip: "esp32s31".to_owned(),
                        model: "esp32s31-mosaico".to_owned(),
                        zone: "desk".to_owned(),
                        group_id: "home-default".to_owned(),
                        vp_version: 0,
                        multinet_ready: true,
                        wake_config: None,
                        capabilities: vec![
                            "device.display_show".to_owned(),
                            "device.audio_set_volume".to_owned(),
                            "device.sensor_read".to_owned(),
                            "device.status_read".to_owned(),
                        ],
                    }),
                },
            )
            .await;

            let ack_env = recv_envelope(&mut ws).await.expect("hello_ack");
            assert_eq!(ack_env.message_type, "hello_ack");

            let snap = harness.handle.snapshot().await.expect("snapshot");
            let conn = snap.devices[0].connection.as_ref().expect("online");
            let console = conn.console.as_ref().expect("native console snapshot");
            assert_eq!(console.api_prefix, "/api/v1");
            assert_eq!(console.mode, assistant_protocol::DeviceConsoleModeSnapshot::Native);
            assert!(console.direct_url.is_none());
            assert!(harness.handle.has_native_console(&device_id).await);
            assert_eq!(conn.mcp_server_keys.len(), 1);
            let expected_mcp_key = conn.mcp_server_keys[0].clone();

            // Verify real HostMcpConnectionFactory + rmcp connects to loopback MCP and discovers all 4 tools
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let cfg = harness
                        .runtime
                        .get_mcp_configuration(GetMcpConfigurationRequest {})
                        .await
                        .expect("get mcp config");
                    if cfg
                        .snapshot
                        .servers
                        .iter()
                        .any(|s| s.server_key == expected_mcp_key && s.tool_count == 4)
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("native loopback MCP should connect and list 4 tools");

            // Spawn simulated Mosaico loop answering WSS device_command, wake_config_sync & voiceprint_sync
            let ws_task = tokio::spawn(async move {
                let mut saw_cmd = false;
                let mut saw_vp = false;
                let mut saw_wake_sync = false;
                while let Some(env) = recv_envelope(&mut ws).await {
                    match env.message_type.as_str() {
                        "device_command" => {
                            let req: crate::device::protocol::DeviceCommandRequest =
                                decode_payload(env);
                            saw_cmd = true;
                            let result_json = if req.capability == "device.status_read" {
                                r#"{"rssi":-52,"volume":68,"battery_pct":91,"temperature_c":31.5,"free_int":70120,"vp_version":0}"#.to_owned()
                            } else {
                                r#"{"ok":true,"emotion":"happy"}"#.to_owned()
                            };
                            send_json(
                                &mut ws,
                                &format!("res-{}", req.command_id),
                                "device_command_result",
                                &DeviceCommandResult {
                                    command_id: req.command_id,
                                    ok: true,
                                    result_json,
                                    media_url: String::new(),
                                    error_code: String::new(),
                                    error_message: String::new(),
                                },
                            )
                            .await;
                        }
                        "wake_config_sync" => {
                            let sync: crate::device::protocol::WakeConfigSyncRequest =
                                decode_payload(env);
                            if sync.config.mode == crate::device::protocol::WakeMode::VoiceprintDirect {
                                saw_wake_sync = true;
                            }
                            send_json(
                                &mut ws,
                                &format!("wake-ack-{}", sync.sync_id),
                                "wake_config_ack",
                                &WakeConfigAck {
                                    sync_id: sync.sync_id,
                                    ok: true,
                                    active_config: Some(sync.config),
                                    error: None,
                                },
                            )
                            .await;
                            if saw_vp && saw_wake_sync {
                                break;
                            }
                        }
                        "voiceprint_sync" => {
                            let sync: crate::device::protocol::VoiceprintSyncRequest =
                                decode_payload(env);
                            saw_vp = true;
                            send_json(
                                &mut ws,
                                "vp-ack-1",
                                "voiceprint_sync_ack",
                                &VoiceprintSyncAck {
                                    vp_version: sync.vp_version,
                                    count: sync.upserts.len() as u32,
                                    ok: true,
                                    error: None,
                                },
                            )
                            .await;
                        }
                        _ => {}
                    }
                }
                let _ = ws.close(None).await;
                (saw_cmd, saw_vp, saw_wake_sync)
            });

            // 1. GET /api/v1/devices triggers live status_read over WSS
            let dev_res = harness
                .handle
                .handle_native_console(
                    &device_id,
                    "api/v1/devices",
                    axum::http::Method::GET,
                    axum::body::Bytes::new(),
                    &harness.speech,
                )
                .await;
            assert_eq!(dev_res.status(), axum::http::StatusCode::OK);
            let dev_bytes = axum::body::to_bytes(dev_res.into_body(), 1024 * 1024)
                .await
                .expect("devices body");
            let dev_json: serde_json::Value =
                serde_json::from_slice(&dev_bytes).expect("devices json");
            assert_eq!(dev_json["devices"][0]["shadow"]["battery_pct"], 91);
            assert_eq!(dev_json["devices"][0]["shadow"]["volume"], 68);

            // 2. POST /api/v1/devices/{id}/commands executes display_show over WSS
            let cmd_body = serde_json::to_vec(&serde_json::json!({
                "agent_id": "ez-assistant",
                "capability": "device.display_show",
                "action": "show",
                "params_json": r#"{"emotion":"happy","text":"Hello"}"#,
                "timeout_ms": 3000
            }))
            .expect("cmd body");
            let cmd_res = harness
                .handle
                .handle_native_console(
                    &device_id,
                    "api/v1/devices/30:ed:a0:f4:5b:f0/commands",
                    axum::http::Method::POST,
                    axum::body::Bytes::from(cmd_body),
                    &harness.speech,
                )
                .await;
            assert_eq!(cmd_res.status(), axum::http::StatusCode::OK);
            let cmd_bytes = axum::body::to_bytes(cmd_res.into_body(), 1024 * 1024)
                .await
                .expect("cmd bytes");
            let cmd_json: serde_json::Value =
                serde_json::from_slice(&cmd_bytes).expect("cmd json");
            assert_eq!(cmd_json["ok"], true);

            // 2b. Loopback MCP endpoint enforces Bearer auth, dispatches tools/call over WSS, and enforces ACL
            let http_client = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("reqwest");
            let mcp_url = format!("http://127.0.0.1:{mcp_port}/mcp/{}", device_id.as_str());
            let unauth = http_client
                .post(&mcp_url)
                .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
                .send()
                .await
                .expect("unauth mcp");
            assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);

            let mcp_call: Value = http_client
                .post(&mcp_url)
                .bearer_auth(&mcp_token)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "device.display_show",
                        "arguments": { "emotion": "excited", "text": "MCP OK" }
                    }
                }))
                .send()
                .await
                .expect("mcp call")
                .json()
                .await
                .expect("mcp call json");
            assert_eq!(mcp_call["result"]["isError"], false);

            // Add deny ACL rule -> both console command & MCP tools/call are rejected
            let deny_body = serde_json::to_vec(&json!({
                "agent_id": "ez-assistant",
                "group_id": "*",
                "zone_pattern": "*",
                "device_id": "*",
                "capability_pattern": "device.display_show",
                "min_speaker_role": 1,
                "allow": false
            }))
            .expect("deny body");
            let deny_res = harness
                .handle
                .handle_native_console(
                    &device_id,
                    "api/v1/acls/deny-display",
                    axum::http::Method::PUT,
                    axum::body::Bytes::from(deny_body),
                    &harness.speech,
                )
                .await;
            assert_eq!(deny_res.status(), axum::http::StatusCode::OK);

            let denied_call: Value = http_client
                .post(&mcp_url)
                .bearer_auth(&mcp_token)
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "tools/call",
                    "params": {
                        "name": "device.display_show",
                        "arguments": { "emotion": "happy", "text": "Blocked" }
                    }
                }))
                .send()
                .await
                .expect("denied mcp call")
                .json()
                .await
                .expect("denied mcp call json");
            assert_eq!(denied_call["result"]["isError"], true);

            // 3. Enroll speaker -> pushes voiceprint_sync over WSS
            let pcm: Vec<u8> = (0..16_000_usize)
                .flat_map(|i| {
                    let s = ((2.0 * std::f32::consts::PI * 220.0 * (i as f32) / 16_000.0).sin()
                        * 20_000.0) as i16;
                    s.to_le_bytes()
                })
                .collect();
            let enroll_body = serde_json::to_vec(&serde_json::json!({
                "speaker_id": "spk_luke",
                "display_name": "Luke",
                "role": 1,
                "audio_base64": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &pcm),
            }))
            .expect("enroll body");
            let enroll_res = harness
                .handle
                .handle_native_console(
                    &device_id,
                    "api/v1/voice/enroll",
                    axum::http::Method::POST,
                    axum::body::Bytes::from(enroll_body),
                    &harness.speech,
                )
                .await;
            assert_eq!(enroll_res.status(), axum::http::StatusCode::OK);

            // 4. PUT /api/v1/wake switches to voiceprint_direct + custom wake word over WSS
            let wake_body = serde_json::to_vec(&json!({
                "mode": "voiceprint_direct",
                "wake_word": "小元同学",
                "wake_pinyin": "xiao yuan tong xue",
                "vp_threshold": 0.78
            }))
            .expect("wake body");
            let wake_res = harness
                .handle
                .handle_native_console(
                    &device_id,
                    "api/v1/wake",
                    axum::http::Method::PUT,
                    axum::body::Bytes::from(wake_body),
                    &harness.speech,
                )
                .await;
            assert_eq!(wake_res.status(), axum::http::StatusCode::OK);
            let wake_bytes = axum::body::to_bytes(wake_res.into_body(), 1024 * 1024)
                .await
                .expect("wake bytes");
            let wake_json: Value = serde_json::from_slice(&wake_bytes).expect("wake json");
            assert_eq!(wake_json["synced"], true);
            assert_eq!(wake_json["wake"]["mode"], "voiceprint_direct");

            let (saw_cmd, saw_vp, saw_wake_sync) = ws_task.await.expect("ws task");
            assert!(saw_cmd);
            assert!(saw_vp);
            assert!(saw_wake_sync);
            mcp_server.abort();
            let _ = mcp_server.await;
            harness.shutdown().await;
        }
    }
}
