import { createHash, randomBytes, randomInt, timingSafeEqual } from "node:crypto";
import type { IncomingMessage } from "node:http";
import type { TLSSocket } from "node:tls";
import WebSocket, { type RawData } from "ws";

import {
  authenticationTranscript,
  pairingAssociatedData,
  pairingBindTranscript,
  pairingCommitTranscript,
  startPartyA,
} from "./crypto.js";
import { type PairedCredential, SimulatorIdentity } from "./identity.js";
import {
  type SimulatorFault,
  isImmediateFault,
} from "./faults.js";
import {
  type DeviceCapabilities,
  type Envelope,
  type OutputPreference,
  base64Url,
  decodeDownlinkPcmFrame,
  decodeEnvelope,
  encodeEnvelope,
  encodeUplinkPcmFrame,
  envelope,
  fromBase64Url,
  isRecord,
  PCM_PAYLOAD_BYTES,
} from "./protocol.js";
import { type DiscoveredHost, SimulatorState } from "./state.js";

const MAX_CONTROL_BYTES = 64 * 1024;

class DeviceGatewayRejection extends Error {
  constructor(readonly code: string) {
    super(`Host rejected device message: ${code}`);
    this.name = "DeviceGatewayRejection";
  }
}

interface AuthChallenge {
  connection_id: string;
  nonce: string;
}

interface ConnectionTarget {
  host: DiscoveredHost;
  capabilities: DeviceCapabilities;
  preference: OutputPreference;
}

export interface PlaybackObserver {
  start(playback: {
    outputId: string;
    runId: string;
    streamId: number;
    text: string;
    sampleCount: number;
  }): void;
  frame(pcmS16Le: Uint8Array): void;
  end(playback: { outputId: string; streamId: number; reason: string }): void;
}

export class DeviceConnection {
  private socket: WebSocket | undefined;
  private lastTextSubmission: { clientInputId: string; text: string; preference: OutputPreference } | undefined;
  private lastPcmSubmission: { clientInputId: string; pcm: Uint8Array; preference: OutputPreference } | undefined;
  private playback: {
    outputId: string;
    runId: string;
    streamId: number;
    expectedBytes: number;
    receivedBytes: number;
    nextSequence: number;
  } | undefined;
  private playbackObserver: PlaybackObserver | undefined;
  private armedFault: SimulatorFault | undefined;
  private pairingCode: string | undefined;
  private connectionTarget: ConnectionTarget | undefined;

  constructor(
    private readonly identity: SimulatorIdentity,
    private readonly state: SimulatorState,
  ) {}

  setPlaybackObserver(observer: PlaybackObserver): void {
    this.playbackObserver = observer;
  }

  injectFault(fault: SimulatorFault): void {
    if (!isImmediateFault(fault)) {
      this.armedFault = fault;
      this.state.setArmedFault(fault);
      this.state.diagnostic("fault_armed", fault);
      return;
    }
    if (fault === "pause_read_5s") {
      const socket = this.requireAuthenticatedSocket();
      socket.pause();
      this.state.diagnostic("fault_injected", fault);
      setTimeout(() => {
        if (this.socket === socket && socket.readyState === WebSocket.OPEN) socket.resume();
      }, 5_000).unref();
      return;
    }
    if (fault === "unsupported_output_preference") {
      const socket = this.requireAuthenticatedSocket();
      const capabilities = this.state.snapshot().capabilities;
      const preference = !capabilities.output_pcm16_16k_mono
        ? "audio"
        : !capabilities.output_text
          ? "text"
          : undefined;
      if (!preference) throw new Error("请先切换为能力受限的终端形态");
      socket.send(encodeEnvelope(envelope("set_output_preference", {
        output_preference: preference,
      })));
      this.state.diagnostic("fault_injected", `${fault} / ${preference}`);
      return;
    }
    this.sendPlaybackCancel(true);
    this.state.diagnostic("fault_injected", fault);
  }

  async connect(
    host: DiscoveredHost,
    capabilities: DeviceCapabilities,
    preference: OutputPreference,
  ): Promise<void> {
    this.disconnect();
    this.connectionTarget = { host, capabilities, preference };
    this.state.clearTransient();
    this.state.patch({
      phase: "connecting",
      selectedHostInstallationId: host.installationId,
    });
    const credential = this.identity.credential;
    const expectedFingerprint = credential?.hostInstallationId === host.installationId
      ? credential.hostCertificateFingerprint
      : host.certificateFingerprint;
    const socket = await openPinnedWebSocket(host, expectedFingerprint);
    this.socket = socket;
    const challengeEnvelope = await nextEnvelope(socket);
    if (challengeEnvelope.type !== "auth_challenge" || !isRecord(challengeEnvelope.payload)) {
      throw new Error("Host did not send auth_challenge");
    }
    const challenge = challengeEnvelope.payload as unknown as AuthChallenge;
    if (credential?.hostInstallationId === host.installationId) {
      try {
        await this.authenticate(socket, challenge, credential, capabilities, preference);
      } catch (error) {
        if (!(error instanceof DeviceGatewayRejection) || error.code !== "device_revoked") {
          throw error;
        }
        await this.reenterPairingAfterRevocation(
          socket,
          credential.deviceId,
          { host, capabilities, preference },
        );
      }
    } else {
      await this.pair(socket, host, capabilities, preference);
    }
  }

  disconnect(): void {
    this.finishObservedPlayback("disconnected");
    this.socket?.close(1000, "simulator disconnect");
    this.socket = undefined;
    this.playback = undefined;
    this.state.patch({ phase: "disconnected" });
  }

  cancelPlayback(): void {
    this.sendPlaybackCancel(false);
  }

  resetPairingSession(): void {
    this.pairingCode = undefined;
  }

  private sendPlaybackCancel(duplicate: boolean): void {
    const socket = this.requireAuthenticatedSocket();
    const playback = this.playback;
    if (!playback) throw new Error("there is no active playback");
    if (!this.state.snapshot().capabilities.playback_cancel) {
      throw new Error("Host did not negotiate playback cancellation");
    }
    const payload = {
      output_id: playback.outputId,
      stream_id: playback.streamId,
    };
    socket.send(encodeEnvelope(envelope("playback_cancel", payload)));
    if (duplicate) socket.send(encodeEnvelope(envelope("playback_cancel", payload)));
    this.state.diagnostic("playback_cancel", playback.outputId);
  }

  setOutputPreference(preference: OutputPreference): void {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) {
      this.state.patch({ outputPreference: preference });
      return;
    }
    socket.send(encodeEnvelope(envelope("set_output_preference", {
      output_preference: preference,
    })));
  }

  submitText(text: string, clientInputId = base64Url(randomBytes(18))): string {
    const socket = this.requireAuthenticatedSocket();
    const normalized = text.trim();
    if (normalized.length === 0) throw new Error("text input must not be blank");
    const preference = this.state.snapshot().outputPreference;
    this.lastTextSubmission = { clientInputId, text: normalized, preference };
    this.state.beginTextInput(clientInputId, normalized);
    const encoded = encodeEnvelope(envelope("text_input", {
      client_input_id: clientInputId,
      text: normalized,
      output_preference: preference,
    }));
    socket.send(encoded);
    if (this.consumeFault("duplicate_next_text_envelope")) socket.send(encoded);
    if (this.consumeFault("disconnect_after_next_input")) {
      setImmediate(() => socket.terminate());
    }
    this.state.diagnostic("input", `submitted ${clientInputId}`);
    return clientInputId;
  }

  retryLastText(): void {
    const submission = this.lastTextSubmission;
    if (!submission) throw new Error("there is no text input to retry");
    const socket = this.requireAuthenticatedSocket();
    this.state.beginTextInput(submission.clientInputId, submission.text);
    socket.send(encodeEnvelope(envelope("text_input", {
      client_input_id: submission.clientInputId,
      text: submission.text,
      output_preference: submission.preference,
    })));
    this.state.diagnostic("input_retry", `retried ${submission.clientInputId}`);
  }

  submitPcm(pcmS16Le: Uint8Array, clientInputId = base64Url(randomBytes(18))): string {
    const preference = this.state.snapshot().outputPreference;
    this.lastPcmSubmission = {
      clientInputId,
      pcm: Uint8Array.from(pcmS16Le),
      preference,
    };
    this.sendPcm(pcmS16Le, clientInputId, preference);
    return clientInputId;
  }

  retryLastPcm(): void {
    const submission = this.lastPcmSubmission;
    if (!submission) throw new Error("there is no PCM input to retry");
    this.sendPcm(submission.pcm, submission.clientInputId, submission.preference);
  }

  private sendPcm(
    pcmS16Le: Uint8Array,
    clientInputId: string,
    preference: OutputPreference,
  ): void {
    const socket = this.requireAuthenticatedSocket();
    const snapshot = this.state.snapshot();
    if (!snapshot.capabilities.input_pcm16_16k_mono) {
      throw new Error("Host did not negotiate PCM input");
    }
    if (pcmS16Le.byteLength === 0 || pcmS16Le.byteLength % PCM_PAYLOAD_BYTES !== 0) {
      throw new Error("PCM input must contain complete 20 ms frames");
    }
    const streamId = randomInt(1, 0xffff_ffff);
    this.state.beginSpeechInput(clientInputId);
    socket.send(encodeEnvelope(envelope("listen_start", {
      client_input_id: clientInputId,
      stream_id: streamId,
      format: {
        encoding: "pcm_s16le",
        sample_rate_hz: 16_000,
        channels: 1,
        frame_duration_ms: 20,
      },
      output_preference: preference,
    })));
    let sequence = 0;
    for (let offset = 0; offset < pcmS16Le.byteLength; offset += PCM_PAYLOAD_BYTES) {
      const wireSequence = sequence === 0 && this.consumeFault("invalid_next_pcm_sequence")
        ? 1
        : sequence;
      socket.send(encodeUplinkPcmFrame(
        streamId,
        wireSequence,
        pcmS16Le.subarray(offset, offset + PCM_PAYLOAD_BYTES),
      ));
      sequence += 1;
    }
    socket.send(encodeEnvelope(envelope("listen_stop", {
      stream_id: streamId,
      last_sequence: sequence - 1,
    })));
    if (this.consumeFault("disconnect_after_next_input")) {
      setImmediate(() => socket.terminate());
    }
    this.state.diagnostic("pcm_input", `${clientInputId} / ${sequence} frames`);
  }

  private async pair(
    socket: WebSocket,
    host: DiscoveredHost,
    capabilities: DeviceCapabilities,
    preference: OutputPreference,
  ): Promise<void> {
    this.state.clearPairedDevice();
    const pairingCode = this.pairingCode
      ?? randomInt(0, 1_000_000).toString().padStart(6, "0");
    this.pairingCode = pairingCode;
    const pairingRequestId = base64Url(randomBytes(18));
    const deviceNonce = base64Url(randomBytes(32));
    const pake = startPartyA(pairingCode, pairingRequestId, host.installationId);
    this.state.patch({ phase: "pairing", pairingCode });
    this.state.diagnostic("pairing", `pending request ${pairingRequestId}`);
    socket.send(encodeEnvelope(envelope("pairing_hello", {
      pairing_request_id: pairingRequestId,
      display_name: "Node 模拟终端",
      device_nonce: deviceNonce,
      capabilities,
      pake_share: base64Url(pake.share),
    })));
    const pending = await expect(socket, "pairing_pending");
    requireString(pending, "pairing_request_id", pairingRequestId);
    const hostPake = await expect(socket, "pairing_pake");
    requireString(hostPake, "pairing_request_id", pairingRequestId);
    const hostNonce = requireString(hostPake, "host_nonce");
    const associatedData = pairingAssociatedData(
      pairingRequestId,
      host.installationId,
      host.certificateFingerprint,
      deviceNonce,
      hostNonce,
      capabilities,
    );
    const keys = pake.finish(
      fromBase64Url(requireString(hostPake, "pake_share"), 65),
      associatedData,
    );
    keys.verifyHostConfirmation(
      fromBase64Url(requireString(hostPake, "confirmation_mac"), 32),
    );
    socket.send(encodeEnvelope(envelope("pairing_confirm", {
      pairing_request_id: pairingRequestId,
      confirmation_mac: base64Url(keys.confirmationMac),
    })));

    const provisional = this.identity.generatePendingCredential(
      "pending",
      host.installationId,
      host.certificateFingerprint,
    );
    const publicKey = fromBase64Url(provisional.publicKey, 32);
    const bindTranscript = pairingBindTranscript(associatedData, publicKey);
    socket.send(encodeEnvelope(envelope("pairing_bind", {
      pairing_request_id: pairingRequestId,
      public_key: provisional.publicKey,
      signature: base64Url(this.identity.sign(provisional, bindTranscript)),
      binding_mac: base64Url(keys.bindingMac(Buffer.from("device-bind"), bindTranscript)),
    })));
    const bindAck = await expect(socket, "pairing_bind_ack");
    requireString(bindAck, "pairing_request_id", pairingRequestId);
    const deviceId = requireString(bindAck, "device_id");
    const commitTranscript = pairingCommitTranscript(bindTranscript, deviceId);
    const expectedHostProof = keys.bindingMac(Buffer.from("host-bind-ack"), commitTranscript);
    const hostProof = fromBase64Url(requireString(bindAck, "host_proof"), 32);
    if (!timingSafeEqual(hostProof, expectedHostProof)) {
      throw new Error("Host binding proof is invalid");
    }
    const credential: PairedCredential = { ...provisional, deviceId };
    await this.identity.savePending(credential);
    socket.send(encodeEnvelope(envelope("pairing_commit", {
      pairing_request_id: pairingRequestId,
      device_id: deviceId,
      signature: base64Url(this.identity.sign(credential, commitTranscript)),
      binding_mac: base64Url(keys.bindingMac(Buffer.from("device-commit"), commitTranscript)),
    })));
    const complete = await expect(socket, "pairing_complete");
    requireString(complete, "device_id", deviceId);
    await this.identity.activate();
    this.pairingCode = undefined;
    this.state.patch({ phase: "disconnected", pairedDeviceId: deviceId });
    this.state.clearTransient();
    this.state.diagnostic("pairing", `paired as ${deviceId}; reconnecting`);
    socket.close(1000, "pairing complete");
    await this.connect(host, capabilities, preference);
  }

  private async authenticate(
    socket: WebSocket,
    challenge: AuthChallenge,
    credential: PairedCredential,
    capabilities: DeviceCapabilities,
    preference: OutputPreference,
  ): Promise<void> {
    if (typeof challenge.connection_id !== "string" || typeof challenge.nonce !== "string") {
      throw new Error("auth_challenge is invalid");
    }
    const deviceNonce = base64Url(randomBytes(32));
    const transcript = authenticationTranscript(
      challenge.connection_id,
      challenge.nonce,
      credential.deviceId,
      deviceNonce,
      capabilities,
      preference,
    );
    const signature = this.identity.sign(credential, transcript);
    if (this.consumeFault("corrupt_next_auth_signature")) {
      signature[0] = (signature[0] ?? 0) ^ 0xff;
    }
    const hello = envelope("hello", {
      device_id: credential.deviceId,
      device_nonce: deviceNonce,
      capabilities,
      output_preference: preference,
      client_version: "device-simulator/0.21.0",
      signature: base64Url(signature),
    });
    if (this.consumeFault("next_protocol_major_mismatch")) hello.protocol_major += 1;
    socket.send(encodeEnvelope(hello));
    const ack = await expect(socket, "hello_ack");
    requireString(ack, "device_id", credential.deviceId);
    if (!isRecord(ack.capabilities)) throw new Error("hello_ack capabilities are invalid");
    const effectiveCapabilities = parseCapabilities(ack.capabilities);
    this.state.patch({
      phase: "idle",
      pairedDeviceId: credential.deviceId,
      outputPreference: preference,
      capabilities: effectiveCapabilities,
    });
    this.state.clearTransient();
    this.state.diagnostic("connection", `authenticated ${credential.deviceId}`);
    socket.on("message", (data, isBinary) => {
      try {
        if (isBinary) {
          this.handlePlaybackFrame(rawBytes(data));
        } else {
          this.handleAuthenticatedEnvelope(decodeEnvelope(rawText(data)));
        }
      } catch (error) {
        this.fail(error);
      }
    });
    socket.on("close", () => {
      if (this.socket === socket) {
        this.finishObservedPlayback("disconnected");
        this.socket = undefined;
        this.state.patch({ phase: "disconnected" });
      }
    });
    socket.on("error", (error) => this.fail(error));
  }

  private handleAuthenticatedEnvelope(message: Envelope): void {
    if (message.type === "ping" && isRecord(message.payload)) {
      if (this.consumeFault("ignore_next_ping")) {
        this.state.diagnostic("fault_injected", "ignore_next_ping");
        return;
      }
      this.socket?.send(encodeEnvelope(envelope("pong", message.payload)));
      return;
    }
    if (message.type === "output_preference_changed" && isRecord(message.payload)) {
      const preference = message.payload.output_preference;
      if (preference === "text" || preference === "audio" || preference === "text_and_audio") {
        this.state.patch({ outputPreference: preference });
      }
      return;
    }
    if (message.type === "input_segment_accepted" && isRecord(message.payload)) {
      const clientInputId = requireString(message.payload, "client_input_id");
      const streamId = requirePositiveInteger(message.payload, "stream_id", 0xffff_ffff);
      const current = this.state.snapshot().currentInteraction;
      if (!current?.segmentClientInputIds?.includes(clientInputId)) {
        throw new Error("input_segment_accepted does not match the current speech interaction");
      }
      this.state.patch({ phase: "recognizing" });
      this.state.diagnostic("input_segment_accepted", `${clientInputId} / ${streamId}`);
      return;
    }
    if (message.type === "input_accepted" && isRecord(message.payload)) {
      const clientInputId = requireString(message.payload, "client_input_id");
      const inputId = requireString(message.payload, "input_id");
      const runId = requireString(message.payload, "run_id");
      const queueState = requireString(message.payload, "queue_state");
      const current = this.state.snapshot().currentInteraction;
      if (!current || current.clientInputId !== clientInputId) {
        throw new Error("input_accepted does not match the current interaction");
      }
      this.state.patch({
        phase: isTerminalQueueState(queueState) ? "idle" : "accepted_or_queued",
        currentInteraction: {
          ...current,
          inputId,
          runId,
          queueState,
        },
      });
      this.state.diagnostic("input_accepted", `${inputId} / ${runId}`);
      return;
    }
    if (message.type === "transcript" && isRecord(message.payload)) {
      const clientInputId = requireString(message.payload, "client_input_id");
      const text = requireString(message.payload, "text");
      const current = this.state.snapshot().currentInteraction;
      if (current?.clientInputId === clientInputId) {
        this.state.patch({
          phase: "processing",
          currentInteraction: { ...current, submittedText: text },
        });
      }
      this.state.diagnostic("transcript", `${clientInputId} / ${text.length} chars`);
      return;
    }
    if (message.type === "text_output" && isRecord(message.payload)) {
      const outputId = requireString(message.payload, "output_id");
      const runId = requireString(message.payload, "run_id");
      const text = requireString(message.payload, "text");
      const current = this.state.snapshot().currentInteraction;
      if (!current || current.runId !== runId) {
        this.state.diagnostic("stale_text_output", outputId);
        return;
      }
      this.state.patch({
        phase: "idle",
        currentInteraction: { ...current, outputId, textOutput: text },
      });
      this.state.diagnostic("text_output", `${outputId} / ${runId}`);
      return;
    }
    if (message.type === "playback_start" && isRecord(message.payload)) {
      const outputId = requireString(message.payload, "output_id");
      const runId = requireString(message.payload, "run_id");
      const text = requireString(message.payload, "text");
      const streamId = requirePositiveInteger(message.payload, "stream_id", 0xffff_ffff);
      const sampleCount = requirePositiveInteger(
        message.payload,
        "sample_count",
        60 * 16_000,
      );
      if (!isRecord(message.payload.format) || !isPcmFormat(message.payload.format)) {
        throw new Error("playback format is invalid");
      }
      this.playback = {
        outputId,
        runId,
        streamId,
        expectedBytes: sampleCount * 2,
        receivedBytes: 0,
        nextSequence: 0,
      };
      this.state.patch({
        phase: "speaking",
        currentPlayback: {
          outputId,
          runId,
          streamId,
          text,
          sampleCount,
          receivedBytes: 0,
          status: "playing",
        },
      });
      this.playbackObserver?.start({ outputId, runId, streamId, text, sampleCount });
      this.state.diagnostic("playback_start", `${outputId} / ${sampleCount} samples`);
      return;
    }
    if (message.type === "playback_end" && isRecord(message.payload)) {
      const outputId = requireString(message.payload, "output_id");
      const streamId = requirePositiveInteger(message.payload, "stream_id", 0xffff_ffff);
      const reason = requireString(message.payload, "reason");
      const playback = this.playback;
      if (!playback || playback.outputId !== outputId || playback.streamId !== streamId) {
        throw new Error("playback_end does not match the active playback");
      }
      if (reason === "completed" && playback.receivedBytes !== playback.expectedBytes) {
        throw new Error("completed playback did not contain the declared sample count");
      }
      const current = this.state.snapshot().currentPlayback;
      if (!current) throw new Error("playback state is unavailable");
      this.playbackObserver?.end({ outputId, streamId, reason });
      this.playback = undefined;
      this.state.patch({
        phase: "idle",
        currentPlayback: {
          ...current,
          receivedBytes: playback.receivedBytes,
          status: reason === "completed" ? "completed" : reason === "cancelled" ? "cancelled" : "failed",
          reason,
        },
      });
      this.state.diagnostic("playback_end", `${outputId} / ${reason}`);
      return;
    }
    if (message.type === "state_changed" && isRecord(message.payload)) {
      const state = requireString(message.payload, "state");
      const runId = typeof message.payload.run_id === "string" ? message.payload.run_id : "pre-run";
      this.state.diagnostic("state_changed", `${runId} / ${state}`);
      if (state === "listening" || state === "recognizing" || state === "idle") {
        this.state.patch({ phase: state });
      } else if (state === "unavailable") {
        this.state.patch({ phase: "idle" });
      }
      return;
    }
    if (message.type === "error" && isRecord(message.payload)) {
      const code = typeof message.payload.code === "string" ? message.payload.code : "device_error";
      this.state.diagnostic("wire_error", code);
      if (code.startsWith("asr_") || code.startsWith("audio_") || code.startsWith("pcm_")) {
        this.state.patch({ phase: "idle", lastError: code });
      }
      if (code === "device_revoked") {
        const socket = this.socket;
        const credential = this.identity.credential;
        const target = this.connectionTarget;
        if (socket && credential && target) {
          void this.reenterPairingAfterRevocation(socket, credential.deviceId, target)
            .catch((error: unknown) => this.fail(error));
        } else {
          void this.identity.clearCredential().catch((error: unknown) => this.fail(error));
        }
      }
    }
  }

  private async reenterPairingAfterRevocation(
    socket: WebSocket,
    deviceId: string,
    target: ConnectionTarget,
  ): Promise<void> {
    await this.identity.clearCredential();
    this.pairingCode = undefined;
    if (this.socket === socket) this.socket = undefined;
    socket.close(1000, "device credential revoked");
    this.state.patch({ phase: "disconnected" });
    this.state.clearPairedDevice();
    this.state.clearTransient();
    this.state.diagnostic("identity_revoked", deviceId);
    await this.connect(target.host, target.capabilities, target.preference);
  }

  private handlePlaybackFrame(data: Uint8Array): void {
    const frame = decodeDownlinkPcmFrame(data);
    const playback = this.playback;
    if (!playback) throw new Error("PCM arrived without playback_start");
    if (frame.streamId !== playback.streamId || frame.sequence !== playback.nextSequence) {
      throw new Error("downlink PCM stream or sequence is invalid");
    }
    if (playback.receivedBytes >= playback.expectedBytes) {
      throw new Error("downlink PCM exceeded the declared sample count");
    }
    const acceptedBytes = Math.min(
      frame.payload.byteLength,
      playback.expectedBytes - playback.receivedBytes,
    );
    playback.receivedBytes += acceptedBytes;
    this.playbackObserver?.frame(frame.payload.subarray(0, acceptedBytes));
    playback.nextSequence += 1;
    const current = this.state.snapshot().currentPlayback;
    if (current && (
      playback.nextSequence % 10 === 0
      || playback.receivedBytes === playback.expectedBytes
    )) {
      this.state.patch({
        currentPlayback: { ...current, receivedBytes: playback.receivedBytes },
      });
    }
  }

  private fail(error: unknown): void {
    this.finishObservedPlayback("connection_error");
    const message = error instanceof Error ? error.message : String(error);
    this.state.patch({ phase: "error", lastError: message });
    this.state.diagnostic("error", message);
  }

  private finishObservedPlayback(reason: string): void {
    const playback = this.playback;
    if (!playback) return;
    this.playbackObserver?.end({
      outputId: playback.outputId,
      streamId: playback.streamId,
      reason,
    });
    this.playback = undefined;
    const current = this.state.snapshot().currentPlayback;
    if (current) {
      this.state.patch({
        currentPlayback: {
          ...current,
          receivedBytes: playback.receivedBytes,
          status: "failed",
          reason,
        },
      });
    }
  }

  private requireAuthenticatedSocket(): WebSocket {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN || this.state.snapshot().phase === "pairing") {
      throw new Error("device is not authenticated");
    }
    return socket;
  }

  private consumeFault(expected: SimulatorFault): boolean {
    if (this.armedFault !== expected) return false;
    this.armedFault = undefined;
    this.state.setArmedFault(undefined);
    this.state.diagnostic("fault_injected", expected);
    return true;
  }
}

function parseCapabilities(value: Record<string, unknown>): DeviceCapabilities {
  const keys: Array<keyof DeviceCapabilities> = [
    "input_text",
    "input_pcm16_16k_mono",
    "output_text",
    "output_pcm16_16k_mono",
    "playback_cancel",
    "display_status",
    "display_transcript",
  ];
  const result = {} as DeviceCapabilities;
  for (const key of keys) {
    if (typeof value[key] !== "boolean") throw new Error(`capability ${key} is invalid`);
    result[key] = value[key];
  }
  return result;
}

function isTerminalQueueState(state: string): boolean {
  return state === "completed" || state === "failed" || state === "cancelled" || state === "interrupted";
}

function openPinnedWebSocket(host: DiscoveredHost, expectedFingerprint: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    const url = `wss://${host.address}:${host.port}${host.path}`;
    const socket = new WebSocket(url, {
      rejectUnauthorized: false,
      perMessageDeflate: false,
      maxPayload: MAX_CONTROL_BYTES,
      handshakeTimeout: 10_000,
    });
    let pinned = false;
    socket.once("upgrade", (response: IncomingMessage) => {
      const certificate = (response.socket as TLSSocket).getPeerCertificate(true);
      if (!certificate.raw) {
        socket.terminate();
        reject(new Error("Host TLS certificate is unavailable"));
        return;
      }
      const fingerprint = createHash("sha256").update(certificate.raw).digest("hex");
      if (fingerprint !== expectedFingerprint) {
        socket.terminate();
        reject(new Error("Host TLS certificate fingerprint changed"));
        return;
      }
      pinned = true;
    });
    socket.once("open", () => {
      if (!pinned) {
        socket.terminate();
        reject(new Error("Host TLS certificate was not pinned"));
      } else {
        resolve(socket);
      }
    });
    socket.once("error", reject);
  });
}

function nextEnvelope(socket: WebSocket): Promise<Envelope> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("device response timed out")), 30_000);
    socket.once("message", (data, isBinary) => {
      clearTimeout(timer);
      if (isBinary) {
        reject(new Error("unexpected binary message"));
        return;
      }
      try {
        resolve(decodeEnvelope(rawText(data)));
      } catch (error) {
        reject(error);
      }
    });
    socket.once("close", () => {
      clearTimeout(timer);
      reject(new Error("device connection closed"));
    });
  });
}

async function expect(socket: WebSocket, type: string): Promise<Record<string, unknown>> {
  const message = await nextEnvelope(socket);
  if (message.type === "error" && isRecord(message.payload)) {
    throw new DeviceGatewayRejection(String(message.payload.code));
  }
  if (message.type !== type || !isRecord(message.payload)) {
    throw new Error(`expected ${type}, received ${message.type}`);
  }
  return message.payload;
}

function requireString(
  value: Record<string, unknown>,
  key: string,
  expected?: string,
): string {
  const field = value[key];
  if (typeof field !== "string" || (expected !== undefined && field !== expected)) {
    throw new Error(`${key} is invalid`);
  }
  return field;
}

function requirePositiveInteger(
  value: Record<string, unknown>,
  key: string,
  maximum: number,
): number {
  const field = value[key];
  if (!Number.isInteger(field) || typeof field !== "number" || field <= 0 || field > maximum) {
    throw new Error(`${key} is invalid`);
  }
  return field;
}

function isPcmFormat(value: Record<string, unknown>): boolean {
  return value.encoding === "pcm_s16le"
    && value.sample_rate_hz === 16_000
    && value.channels === 1
    && value.frame_duration_ms === 20;
}

function rawText(data: RawData): string {
  if (typeof data === "string") return data;
  if (data instanceof ArrayBuffer) return Buffer.from(data).toString("utf8");
  if (Array.isArray(data)) return Buffer.concat(data).toString("utf8");
  return data.toString("utf8");
}

function rawBytes(data: RawData): Uint8Array {
  if (Array.isArray(data)) return Uint8Array.from(Buffer.concat(data));
  if (data instanceof ArrayBuffer) return new Uint8Array(data.slice(0));
  return Uint8Array.from(data);
}
