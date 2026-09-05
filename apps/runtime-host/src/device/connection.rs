//! 单条 WSS 连接的配对或已登记设备认证状态机。

use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use assistant_protocol::{
    CancelRunRequest, DeviceCapabilitiesSnapshot, DeviceId, OutputPreferenceSnapshot,
    SubmitInputMode, SubmitInputRequest,
};
use assistant_runtime::{
    DeviceInputSource, DeviceLifecycle, DevicePublicKey, InputChannelSource, InputModality,
    NewPairedDevice, OutputPreference, SubmitSessionInputRequest,
};
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    response::Response,
};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::{
    sync::mpsc,
    time::{Instant as TokioInstant, MissedTickBehavior, interval, timeout, timeout_at},
};

use super::{
    crypto::{
        HostPakeState, auth_transcript, decode_base64, encode_base64, pairing_associated_data,
        pairing_bind_transcript, pairing_commit_transcript, random_stream_id, random_token,
        verify_ed25519,
    },
    gateway::{
        ConnectionCommand, DeviceGatewayError, GatewayShared, PairingDecision,
        PlaybackPreparationResult,
    },
    protocol::{
        ApplicationPing, AuthChallenge, DeviceHello, DownlinkPcmFrame, Envelope, HelloAck,
        InputAccepted, InputSegmentAccepted, InteractionStateChanged, ListenCancel, ListenStart,
        ListenStop, MessageReplayWindow, OutputPreferenceChanged, PairingBind, PairingBindAck,
        PairingCommit, PairingComplete, PairingConfirmation, PairingHello, PairingPake,
        PairingPending, PcmFormat, PlaybackCancel, PlaybackEnd, PlaybackStart, ProtocolError,
        SetOutputPreference, TextInput, Transcript, UplinkPcmFrame, WireError,
        preference_is_supported,
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
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade
        .max_message_size(super::protocol::MAX_CONTROL_MESSAGE_BYTES)
        .max_frame_size(super::protocol::MAX_CONTROL_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(error) = serve(socket, shared).await {
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
            pair_device(socket, shared, hello).await
        }
        "hello" => {
            let hello = first.payload::<DeviceHello>()?;
            authenticate_device(socket, shared, replay, connection_id, host_nonce, hello).await
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

async fn pair_device(
    mut socket: WebSocket,
    shared: std::sync::Arc<GatewayShared>,
    hello: PairingHello,
) -> Result<(), ConnectionError> {
    validate_pairing_hello(&hello)?;
    if !shared.pairing_is_open(now_ms()?).await {
        send_wire_error(&mut socket, "pairing_not_open", None, true).await?;
        return close(&mut socket, 1008, "pairing not open").await;
    }
    let device_share = decode_base64(&hello.pake_share, SPAKE2_P256_SHARE_BYTES)?;
    let (decision_tx, mut decision_rx) = mpsc::channel(PAIRING_DECISION_QUEUE_CAPACITY);
    let expires_at_ms = shared
        .register_pending(
            hello.pairing_request_id.clone(),
            hello.display_name.clone(),
            hello.capabilities,
            decision_tx,
        )
        .await?;
    send_payload(
        &mut socket,
        "pairing_pending",
        &PairingPending {
            pairing_request_id: hello.pairing_request_id.clone(),
            expires_at_ms,
        },
    )
    .await?;

    let deadline = tokio::time::Instant::now()
        + Duration::from_millis(
            u64::try_from(expires_at_ms.saturating_sub(now_ms()?)).unwrap_or_default(),
        );
    let decision = timeout_at(deadline, decision_rx.recv())
        .await
        .map_err(|_| ConnectionError::PairingExpired)?
        .ok_or(ConnectionError::PairingCancelled)?;
    complete_pairing(&mut socket, &shared, &hello, device_share, decision).await
}

async fn complete_pairing(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    hello: &PairingHello,
    device_share: Vec<u8>,
    decision: PairingDecision,
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
        &decision.pairing_code,
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
        return close(socket, 1008, "pairing failed").await;
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

    let device_id = DeviceId::new(format!("device-{}", random_token(18)?))
        .map_err(|_| ConnectionError::InvalidMessage)?;
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
    let public_key =
        DevicePublicKey::from_slice(&public_key).ok_or(ConnectionError::InvalidMessage)?;
    let display_name = decision
        .display_name
        .unwrap_or_else(|| hello.display_name.clone());
    let stored = shared
        .runtime
        .register_paired_device(NewPairedDevice {
            device_id: device_id.clone(),
            display_name,
            public_key,
            paired_at_ms: now_ms()?,
        })
        .await?;
    shared.remove_pending(&hello.pairing_request_id).await;
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
    send_payload(
        &mut socket,
        "hello_ack",
        &HelloAck {
            device_id: device_id.to_string(),
            connection_id: connection_id.clone(),
            capabilities,
            output_preference: hello.output_preference,
        },
    )
    .await?;
    let mut signals = shared
        .register_connection(
            device_id.clone(),
            connection_id.clone(),
            capabilities,
            hello.output_preference,
        )
        .await;
    let result = authenticated_loop(
        &mut socket,
        &shared,
        &device_id,
        &connection_id,
        capabilities,
        &mut replay,
        &mut signals,
    )
    .await;
    shared
        .unregister_connection(&device_id, &connection_id)
        .await;
    result
}

async fn authenticated_loop(
    socket: &mut WebSocket,
    shared: &GatewayShared,
    device_id: &DeviceId,
    connection_id: &str,
    capabilities: DeviceCapabilitiesSnapshot,
    replay: &mut MessageReplayWindow,
    commands: &mut mpsc::Receiver<ConnectionCommand>,
) -> Result<(), ConnectionError> {
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
                                eprintln!("event=voice_capture_started ts_ms={} device={} input={} stream_id={}", timestamp_ms(), correlation_id(device_id.as_str()), correlation_id(&client_input_id), request.stream_id);
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
                }
            }
        }
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
                correlation_id(accepted.run_id.as_str()),
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
    let session = route_device_channel(runtime)?;
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
    let target = route_device_channel(runtime)?;
    let session_id = target.session_id;
    let variant = target.current_variant;
    let result = runtime
        .submit_session_input(SubmitSessionInputRequest {
            input: SubmitInputRequest {
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
    Ok(InputAccepted {
        client_input_id: client_input_id.to_owned(),
        input_id: result.input_id,
        run_id: result.run.run_id,
        queue_state: result.run.status,
    })
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
    let target = route_device_channel(shared.runtime.as_ref())?;
    let session_id = target.session_id;
    let variant = target.current_variant;
    let result = shared
        .runtime
        .submit_session_input(SubmitSessionInputRequest {
            input: SubmitInputRequest {
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
    Ok(InputAccepted {
        client_input_id: request.client_input_id,
        input_id: result.input_id,
        run_id: result.run.run_id,
        queue_state: result.run.status,
    })
}

/// 本版本 Device Channel 的 Host 路由策略：选择当前活动 Controller Session。
///
/// Router 每次读取 Runtime 权威 Session 投影，不在 Gateway 内缓存 Session ID 或角色状态。
fn route_device_channel(
    runtime: &assistant_runtime::AssistantRuntime,
) -> Result<assistant_protocol::SessionSummary, assistant_runtime::RuntimeError> {
    runtime
        .list_sessions(assistant_protocol::ListSessionsRequest::default())?
        .sessions
        .into_iter()
        .find(|session| session.role == assistant_protocol::SessionRoleSnapshot::Controller)
        .ok_or(assistant_runtime::RuntimeError::ControllerUnavailable)
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
        assistant_runtime::RuntimeError::ModelUnavailable { .. } => ("model_unavailable", true),
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
}
