import { randomBytes } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";

import WebSocket from "ws";

const repository = path.resolve(import.meta.dirname, "../../..");
const hostBinary = path.join(repository, "target/debug/ez-assistant-runtime");
const simulatorBinary = path.join(repository, "tools/device-simulator/dist/node/main.js");

async function main() {
const milestone = process.env.EZ_DEVICE_E2E_MILESTONE;
const isM6 = milestone === "m6" || milestone === "m7" || milestone === "m8";
const isM7 = milestone === "m7" || milestone === "m8";
const isM8 = milestone === "m8";
const temporaryRoot = await mkdtemp(path.join(tmpdir(), "ez-assistant-m5-e2e."));
const runtimeHome = path.join(temporaryRoot, "runtime");
const simulatorHome = path.join(temporaryRoot, "device");
let host;
let simulator;
let bridge;
let provider;
let hostOutput = () => "";

try {
  provider = await startFakeProvider();
  await writeConfig(runtimeHome, provider.endpoint);
  host = spawn(hostBinary, ["serve", "--runtime-home", runtimeHome], {
    cwd: repository,
    stdio: ["ignore", "pipe", "pipe"],
  });
  hostOutput = captureProcessOutput(host);
  const discovery = await waitFor(async () => JSON.parse(
    await readFile(path.join(runtimeHome, "run/runtime.json"), "utf8"),
  ));
  const command = hostCommand(discovery.address, discovery.access_token);
  const controllerSession = await waitFor(async () => {
    const sessions = await command("runtime", "list_sessions", {});
    return sessions.sessions.find((session) => session.role === "controller") ?? false;
  });
  await command("device_gateway", "set_access_enabled", { enabled: true });
  const opened = await command("device_gateway", "open_pairing_window", {});
  const installationId = opened.snapshot.installation_id;

  simulator = spawn(process.execPath, [simulatorBinary, "--home", simulatorHome, "--port", "0"], {
    cwd: path.dirname(simulatorBinary),
    stdio: ["ignore", "pipe", "pipe"],
  });
  const pageUrl = await waitForLine(simulator.stdout, /^EZ Assistant device simulator: (.+)$/);
  const html = await (await fetch(pageUrl)).text();
  const token = html.match(/name="simulator-token" content="([^"]+)"/)?.[1];
  if (!token) throw new Error("simulator page token is missing");
  bridge = new Bridge(pageUrl, token);
  await bridge.connect();
  await bridge.waitFor((snapshot) =>
    snapshot.hosts.some((candidate) => candidate.installationId === installationId),
  );
  bridge.send({ type: "connect", hostInstallationId: installationId });
  const pairing = await bridge.waitFor((snapshot) =>
    typeof snapshot.pairingCode === "string" ? snapshot : false,
  );
  if (!/^\d{6}$/.test(pairing.pairingCode)) {
    throw new Error("device pairing code must contain exactly six digits");
  }
  const pending = await waitFor(async () => {
    const result = await command("device_gateway", "get_snapshot", {});
    return result.pending_pairings[0] ?? false;
  });
  await command("device_gateway", "confirm_pairing", {
    pairing_request_id: pending.pairing_request_id,
    pairing_code: pairing.pairingCode,
    display_name: "M5 Node 终端",
  });
  const authenticated = await bridge.waitFor((snapshot) =>
    snapshot.phase === "idle" && snapshot.pairedDeviceId ? snapshot : false,
  );
  const gateway = await command("device_gateway", "get_snapshot", {});
  const connected = gateway.devices.find(
    (device) => device.device_id === authenticated.pairedDeviceId,
  );
  if (!connected?.connection?.capabilities?.input_pcm16_16k_mono) {
    throw new Error("Host did not negotiate PCM input after ASR became ready");
  }
  if (isM6 && (
    !connected?.connection?.capabilities?.output_pcm16_16k_mono
    || !connected?.connection?.capabilities?.playback_cancel
  )) {
    throw new Error("Host did not negotiate PCM output and playback cancellation");
  }

  if (isM6) {
    bridge.send({ type: "set_output_preference", preference: "text_and_audio" });
    await bridge.waitFor((snapshot) =>
      snapshot.outputPreference === "text_and_audio" ? snapshot : false,
    );
  }

  bridge.send({ type: "submit_test_pcm" });
  const completed = await bridge.waitFor((snapshot) =>
    snapshot.currentInteraction?.inputId
      && snapshot.currentInteraction?.runId
      && snapshot.currentInteraction?.textOutput === "offline answer"
      && (!isM6 || snapshot.currentPlayback?.status === "completed")
      ? snapshot
      : false,
    20_000,
  );
  const firstInputId = completed.currentInteraction.inputId;
  const firstRunId = completed.currentInteraction.runId;
  if (isM6 && bridge.playbackBytes !== 1_280) {
    throw new Error(`expected 1280 PCM bytes on the H5 bridge, received ${bridge.playbackBytes}`);
  }
  bridge.send({ type: "retry_pcm" });
  const retried = await bridge.waitFor((snapshot) => {
    const accepted = snapshot.diagnostics.filter((entry) => entry.kind === "input_accepted");
    return accepted.length >= 2 ? snapshot : false;
  }, 20_000);
  if (
    retried.currentInteraction.inputId !== firstInputId
    || retried.currentInteraction.runId !== firstRunId
  ) {
    throw new Error("PCM retry did not reuse the first Input/Run");
  }
  bridge.send({ type: "disconnect" });
  await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
  bridge.send({ type: "connect", hostInstallationId: installationId });
  await bridge.waitFor((snapshot) => snapshot.phase === "idle" ? snapshot : false);
  bridge.send({ type: "retry_pcm" });
  const reconnectedRetry = await bridge.waitFor((snapshot) => {
    const accepted = snapshot.diagnostics.filter((entry) => entry.kind === "input_accepted");
    return accepted.length >= 3 ? snapshot : false;
  }, 20_000);
  if (
    reconnectedRetry.currentInteraction.inputId !== firstInputId
    || reconnectedRetry.currentInteraction.runId !== firstRunId
  ) {
    throw new Error("PCM retry after reconnect did not reuse the first Input/Run");
  }

  let continuousInputId;
  let continuousRunId;
  let takeoverRunId;
  let takeoverInputId;
  if (isM8) {
    bridge.send({ type: "set_output_preference", preference: "text" });
    await bridge.waitFor((snapshot) => snapshot.outputPreference === "text" ? snapshot : false);
    const segmentAckBaseline = bridge.latest.diagnostics.filter(
      (entry) => entry.kind === "input_segment_accepted",
    ).length;
    const inputAckBaseline = bridge.latest.diagnostics.filter(
      (entry) => entry.kind === "input_accepted",
    ).length;
    const asrBaseline = provider.asrRequests();

    bridge.send({ type: "submit_test_pcm" });
    await bridge.waitFor((snapshot) =>
      snapshot.diagnostics.filter((entry) => entry.kind === "input_segment_accepted").length
        >= segmentAckBaseline + 1
        ? snapshot
        : false,
    );
    bridge.send({ type: "submit_test_pcm" });
    await bridge.waitFor((snapshot) =>
      snapshot.diagnostics.filter((entry) => entry.kind === "input_segment_accepted").length
        >= segmentAckBaseline + 2
        ? snapshot
        : false,
    );
    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) => snapshot.phase === "idle" ? snapshot : false);
    bridge.send({ type: "retry_pcm" });
    const acceptedContinuous = await bridge.waitFor((snapshot) => {
      const segmentAcks = snapshot.diagnostics.filter(
        (entry) => entry.kind === "input_segment_accepted",
      );
      const inputAcks = snapshot.diagnostics.filter((entry) => entry.kind === "input_accepted");
      return segmentAcks.length >= segmentAckBaseline + 3
        && inputAcks.length >= inputAckBaseline + 1
        ? snapshot
        : false;
    }, 20_000);
    const acceptedRunId = acceptedContinuous.currentInteraction.runId;
    if (!acceptedRunId) throw new Error("continuous speech did not create a Runtime Run");
    let continuousRun;
    try {
      continuousRun = await waitFor(async () => {
        const result = await command("runtime", "get_run", {
          session_id: controllerSession.session_id,
          run_id: acceptedRunId,
        });
        return ["completed", "failed", "cancelled", "interrupted"].includes(result.run.status)
          ? result.run
          : false;
      }, 20_000);
    } catch {
      const stalled = await command("runtime", "get_run", {
        session_id: controllerSession.session_id,
        run_id: acceptedRunId,
      });
      throw new Error(
        `continuous speech Run stalled: ${JSON.stringify(stalled.run)}; model requests: ${provider.modelRequests()}; Host: ${hostOutput()}`,
      );
    }
    if (continuousRun.status !== "completed") {
      throw new Error(
        `continuous speech Run failed: ${JSON.stringify(continuousRun)}; model requests: ${provider.modelRequests()}`,
      );
    }
    const continuous = await bridge.waitFor((snapshot) =>
      snapshot.currentInteraction?.runId === continuousRun.run_id
        && snapshot.currentInteraction?.textOutput === "offline answer"
        ? snapshot
        : false,
    );
    continuousInputId = continuous.currentInteraction.inputId;
    continuousRunId = continuous.currentInteraction.runId;
    if (provider.asrRequests() !== asrBaseline + 2) {
      throw new Error(
        `cross-WSS segment retry triggered duplicate ASR: ${provider.asrRequests() - asrBaseline} new requests`,
      );
    }

    bridge.send({ type: "set_output_preference", preference: "text_and_audio" });
    await bridge.waitFor((snapshot) =>
      snapshot.outputPreference === "text_and_audio" ? snapshot : false,
    );
    const takeoverInputAckBaseline = bridge.latest.diagnostics.filter(
      (entry) => entry.kind === "input_accepted",
    ).length;
    const takeoverSegmentAckBaseline = bridge.latest.diagnostics.filter(
      (entry) => entry.kind === "input_segment_accepted",
    ).length;
    const takeoverPlaybackBaseline = bridge.latest.diagnostics.filter(
      (entry) => entry.kind === "playback_start",
    ).length;
    const takeoverAsrBaseline = provider.asrRequests();
    bridge.send({ type: "submit_text", text: "M8 delayed playback" });
    const runningTakeover = await bridge.waitFor((snapshot) => {
      const inputAcks = snapshot.diagnostics.filter((entry) => entry.kind === "input_accepted");
      return inputAcks.length >= takeoverInputAckBaseline + 1
        && snapshot.currentInteraction?.runId
        ? snapshot
        : false;
    });
    const interruptedRunId = runningTakeover.currentInteraction.runId;
    await waitFor(() => provider.ttsRequests() >= 2, 10_000);
    bridge.send({ type: "set_output_preference", preference: "text" });
    await bridge.waitFor((snapshot) => snapshot.outputPreference === "text" ? snapshot : false);
    bridge.send({ type: "submit_test_pcm" });
    await bridge.waitFor((snapshot) =>
      snapshot.diagnostics.filter((entry) => entry.kind === "input_segment_accepted").length
        >= takeoverSegmentAckBaseline + 1
        ? snapshot
        : false,
    );
    const interruptedRun = await waitFor(async () => {
      const result = await command("runtime", "get_run", {
        session_id: controllerSession.session_id,
        run_id: interruptedRunId,
      });
      return result.run.status === "cancelled" ? result.run : false;
    });
    if (interruptedRun.status !== "cancelled") {
      throw new Error(`PTT did not cancel the active Run: ${JSON.stringify(interruptedRun)}`);
    }
    const takeover = await bridge.waitFor((snapshot) => {
      const inputAcks = snapshot.diagnostics.filter((entry) => entry.kind === "input_accepted");
      return inputAcks.length >= takeoverInputAckBaseline + 2
        && snapshot.currentInteraction?.runId !== interruptedRunId
        && snapshot.currentInteraction?.textOutput === "offline answer"
        ? snapshot
        : false;
    }, 20_000);
    takeoverInputId = takeover.currentInteraction.inputId;
    takeoverRunId = takeover.currentInteraction.runId;
    if (provider.asrRequests() !== takeoverAsrBaseline + 1) {
      throw new Error("PTT takeover did not submit exactly one new speech segment");
    }
    if (provider.cancelledTtsRequests() !== 1) {
      throw new Error(
        `late TTS was not cancelled exactly once: ${provider.cancelledTtsRequests()}`,
      );
    }
    const playbackStarts = takeover.diagnostics.filter(
      (entry) => entry.kind === "playback_start",
    ).length;
    if (playbackStarts !== takeoverPlaybackBaseline) {
      throw new Error("late playback resumed after PTT takeover");
    }
  }

  if (isM7) {
    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "set_terminal_profile", profile: "voice_only" });
    await bridge.waitFor((snapshot) =>
      snapshot.terminalProfile === "voice_only" && snapshot.outputPreference === "audio"
        ? snapshot
        : false,
    );
    bridge.send({ type: "connect", hostInstallationId: installationId });
    const voiceOnly = await bridge.waitFor((snapshot) =>
      snapshot.phase === "idle" && snapshot.capabilities.output_text === false
        ? snapshot
        : false,
    );
    if (
      voiceOnly.capabilities.input_text !== false
      || voiceOnly.capabilities.input_pcm16_16k_mono !== true
      || voiceOnly.capabilities.output_pcm16_16k_mono !== true
    ) {
      throw new Error("Voice-only profile did not negotiate its exact capability set");
    }

    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "set_terminal_profile", profile: "screen_voice" });
    bridge.send({ type: "connect", hostInstallationId: installationId });
    const screenVoice = await bridge.waitFor((snapshot) =>
      snapshot.phase === "idle" && snapshot.terminalProfile === "screen_voice"
        ? snapshot
        : false,
    );
    if (
      screenVoice.capabilities.input_text !== true
      || screenVoice.capabilities.input_pcm16_16k_mono !== true
      || screenVoice.capabilities.output_text !== true
      || screenVoice.capabilities.output_pcm16_16k_mono !== true
    ) {
      throw new Error("Screen voice profile did not negotiate its exact capability set");
    }

    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "set_terminal_profile", profile: "keyboard_screen" });
    await bridge.waitFor((snapshot) =>
      snapshot.terminalProfile === "keyboard_screen"
        && snapshot.outputPreference === "text"
        ? snapshot
        : false,
    );
    bridge.send({ type: "connect", hostInstallationId: installationId });
    const keyboard = await bridge.waitFor((snapshot) =>
      snapshot.phase === "idle" && snapshot.capabilities.input_pcm16_16k_mono === false
        ? snapshot
        : false,
    );
    if (
      keyboard.capabilities.input_text !== true
      || keyboard.capabilities.output_text !== true
      || keyboard.capabilities.output_pcm16_16k_mono !== false
    ) {
      throw new Error("Keyboard screen profile did not negotiate its exact capability set");
    }
    bridge.send({ type: "inject_fault", fault: "unsupported_output_preference" });
    await bridge.waitFor((snapshot) =>
      snapshot.diagnostics.some(
        (entry) => entry.kind === "wire_error" && entry.detail === "unsupported_output_preference",
      )
        ? snapshot
        : false,
    );

    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "inject_fault", fault: "corrupt_next_auth_signature" });
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) => snapshot.phase === "error" ? snapshot : false);
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) => snapshot.phase === "idle" ? snapshot : false);

    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "set_terminal_profile", profile: "mixed" });
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) =>
      snapshot.phase === "idle" && snapshot.terminalProfile === "mixed" ? snapshot : false,
    );
    bridge.send({ type: "inject_fault", fault: "invalid_next_pcm_sequence" });
    bridge.send({ type: "submit_test_pcm" });
    await bridge.waitFor((snapshot) =>
      snapshot.diagnostics.some(
        (entry) => entry.kind === "wire_error" && entry.detail === "invalid_pcm_sequence",
      )
        ? snapshot
        : false,
    );
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) => snapshot.phase === "idle" ? snapshot : false);

    bridge.send({ type: "disconnect" });
    await bridge.waitFor((snapshot) => snapshot.phase === "disconnected" ? snapshot : false);
    await command("device_gateway", "revoke_device", {
      device_id: authenticated.pairedDeviceId,
    });
    await command("device_gateway", "open_pairing_window", {});
    bridge.send({ type: "connect", hostInstallationId: installationId });
    await bridge.waitFor((snapshot) =>
      snapshot.phase === "pairing"
        && /^\d{6}$/.test(snapshot.pairingCode ?? "")
        && snapshot.pairedDeviceId === undefined
        && snapshot.diagnostics.some((entry) => entry.kind === "identity_revoked")
        ? snapshot
        : false,
    );
    const revokedGateway = await command("device_gateway", "get_snapshot", {});
    if (
      !revokedGateway.devices.some(
        (device) => device.device_id === authenticated.pairedDeviceId
          && device.lifecycle === "revoked",
      )
      || revokedGateway.pending_pairings.length !== 1
    ) {
      throw new Error("offline revocation did not preserve the revoked record and reopen pairing");
    }
  }

  const sessions = await command("runtime", "list_sessions", {});
  const controller = sessions.sessions.find((session) => session.role === "controller");
  const page = await command("runtime", "list_conversation_page", {
    owner: { type: "main_session", session_id: controller.session_id },
    cursor: null,
    limit: 100,
  });
  const userMessages = page.snapshot.value.items.filter((item) => item.type === "user");
  if (
    userMessages.length !== (isM8 ? 4 : 1)
    || userMessages[0].text !== "M5 语音输入"
  ) {
    throw new Error("canonical Conversation did not contain the expected device UserMessages");
  }
  if (isM8 && userMessages[1].text !== "M8 连续输入第一段\nM8 连续输入第二段") {
    throw new Error(`continuous speech was not committed once: ${userMessages[1].text}`);
  }
  if (
    isM8
    && (
      userMessages[2].text !== "M8 delayed playback"
      || userMessages[3].text !== "M8 用户接管语音"
    )
  ) {
    throw new Error("PTT takeover inputs were not preserved in the canonical Conversation");
  }
  if (userMessages[0].source?.modality !== "speech_transcript") {
    throw new Error("canonical UserMessage did not preserve speech transcript modality");
  }
  if (isM6 && provider.ttsRequests() !== (isM8 ? 2 : 1)) {
    throw new Error(`unexpected TTS request count: ${provider.ttsRequests()}`);
  }

  console.log(JSON.stringify({
    runtimeHome,
    simulatorHome,
    deviceId: authenticated.pairedDeviceId,
    inputId: firstInputId,
    runId: firstRunId,
    transcript: userMessages[0].text,
    retryReused: true,
    reconnectRetryReused: true,
    playback: isM6 ? completed.currentPlayback : undefined,
    h5BridgePlaybackBytes: isM6 ? bridge.playbackBytes : undefined,
    ttsRequests: provider.ttsRequests(),
    m8ContinuousInput: isM8 ? {
      inputId: continuousInputId,
      runId: continuousRunId,
      segmentAcknowledgements: 3,
      asrRequests: provider.asrRequests(),
      runtimeSubmits: 1,
      crossWssRetryDeduplicated: true,
    } : undefined,
    m8PttTakeover: isM8 ? {
      inputId: takeoverInputId,
      runId: takeoverRunId,
      activeRunCancelled: true,
      lateTtsCancelled: true,
      latePlaybackSuppressed: true,
    } : undefined,
    m7FaultMatrix: isM7 ? {
      terminalProfiles: ["voice_only", "screen_voice", "keyboard_screen", "mixed"],
      unsupportedOutputPreference: true,
      corruptedAuthentication: true,
      invalidPcmSequence: true,
      offlineRevocationReenteredPairing: true,
    } : undefined,
  }, null, 2));
  await command("runtime", "shutdown_runtime", {});
  await waitForExit(host, 10_000);
  host = undefined;
} finally {
  bridge?.close();
  await stopChild(simulator);
  await stopChild(host);
  await provider?.close();
  await rm(temporaryRoot, { recursive: true, force: true });
}
}

async function writeConfig(home, endpoint) {
  await mkdir(home, { recursive: true, mode: 0o700 });
  const document = `schema_version = 1
default_model = "fixture"

[runtime.model_transport]
connect_timeout_ms = 1000
request_timeout_ms = 10000

[models.fixture]
protocol = "openai_chat_completions"
provider = "fixture"
endpoint = "${endpoint}/v1"
model = "offline-model"
api_key = "fixture-model-secret"
context_window_tokens = 8192
max_output_tokens = 4096

[speech.asr]
provider = "dashscope"
model = "fixture-asr"
credential = "fixture-asr-secret"
endpoint = "${endpoint}"
timeout_ms = 5000

[speech.tts]
provider = "dashscope"
model = "fixture-tts"
voice = "fixture-voice"
credential = "fixture-tts-secret"
endpoint = "${endpoint}"
timeout_ms = 5000
`;
  const configPath = path.join(home, "config.toml");
  await writeFile(configPath, document, { mode: 0o600 });
  await chmod(home, 0o700);
  await chmod(configPath, 0o600);
}

async function startFakeProvider() {
  let modelRequests = 0;
  let ttsRequests = 0;
  let cancelledTtsRequests = 0;
  let asrRequests = 0;
  let takeoverToolIssued = false;
  let endpoint;
  const server = createServer(async (request, response) => {
    const chunks = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const requestBody = Buffer.concat(chunks).toString("utf8");
    if (request.url === "/api/v1/services/aigc/multimodal-generation/generation") {
      asrRequests += 1;
      response.writeHead(200, { "content-type": "application/json" });
      const text = asrRequests === 1
        ? "M5 语音输入"
        : asrRequests <= 3
          ? "M5 语音输入"
          : asrRequests === 4
          ? "M8 连续输入第一段"
          : asrRequests === 5
            ? "M8 连续输入第二段"
            : "M8 用户接管语音";
      response.end(JSON.stringify({ output: { text } }));
      return;
    }
    if (request.url === "/v1/chat/completions") {
      modelRequests += 1;
      response.writeHead(200, { "content-type": "text/event-stream" });
      if (!takeoverToolIssued && requestBody.includes("M8 delayed playback")) {
        takeoverToolIssued = true;
        response.end(
          'data: {"id":"m8-takeover-tool","model":"offline-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_m8_takeover_speak","type":"function","function":{"name":"speak","arguments":"{\\"text\\":\\"M8 迟发播放\\"}"}}]},"finish_reason":null}]}\n\n'
          + 'data: {"id":"m8-takeover-tool","model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}\n\n'
          + "data: [DONE]\n\n",
        );
      } else if (modelRequests === 1) {
        response.end(
          'data: {"id":"m6-tool","model":"offline-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_speak_1","type":"function","function":{"name":"speak","arguments":"{\\"text\\":\\"最终压缩播报。\\"}"}}]},"finish_reason":null}]}\n\n'
          + 'data: {"id":"m6-tool","model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}\n\n'
          + "data: [DONE]\n\n",
        );
      } else {
        const responseId = `m5-${modelRequests}`;
        response.end(
          `data: {"id":"${responseId}","model":"offline-model","choices":[{"index":0,"delta":{"role":"assistant","content":"offline answer"},"finish_reason":null}]}\n\n`
          + `data: {"id":"${responseId}","model":"offline-model","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":2,"total_tokens":22}}\n\n`
          + "data: [DONE]\n\n",
        );
      }
      return;
    }
    if (request.url === "/api/v1/services/audio/tts/SpeechSynthesizer") {
      ttsRequests += 1;
      if (requestBody.includes("M8 迟发播放")) {
        let completed = false;
        response.once("close", () => {
          if (!completed) cancelledTtsRequests += 1;
        });
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        if (response.destroyed) return;
        completed = true;
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ output: { audio: { url: `${endpoint}/fixture-tts.pcm` } } }));
      return;
    }
    if (request.url === "/fixture-tts.pcm") {
      response.writeHead(200, { "content-type": "application/octet-stream" });
      response.end(Buffer.alloc(1_280, 7));
      return;
    }
    response.writeHead(404).end();
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  endpoint = `http://127.0.0.1:${address.port}`;
  return {
    endpoint,
    asrRequests: () => asrRequests,
    cancelledTtsRequests: () => cancelledTtsRequests,
    modelRequests: () => modelRequests,
    ttsRequests: () => ttsRequests,
    close: () => new Promise((resolve) => server.close(resolve)),
  };
}

function hostCommand(baseUrl, token) {
  let next = 1;
  return async (scope, type, payload) => {
    const response = await fetch(`${baseUrl}/commands`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify({
        request_id: `m5-${next++}-${randomBytes(4).toString("hex")}`,
        command: { scope, payload: { type, payload } },
      }),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(`Host command ${type} failed: ${JSON.stringify(body)}`);
    return body.result.payload.payload;
  };
}

class Bridge {
  constructor(pageUrl, token) {
    this.url = new URL(`/bridge?token=${encodeURIComponent(token)}`, pageUrl);
    this.url.protocol = "ws:";
    this.waiters = [];
    this.latest = undefined;
    this.playbackBytes = 0;
  }

  async connect() {
    this.socket = new WebSocket(this.url, { origin: this.url.origin.replace("ws:", "http:") });
    this.socket.on("message", (data, isBinary) => {
      if (isBinary) {
        this.playbackBytes += data.byteLength;
        return;
      }
      const message = JSON.parse(data.toString("utf8"));
      if (message.type !== "snapshot") return;
      this.latest = message.payload;
      for (const waiter of this.waiters.slice()) {
        const result = waiter.predicate(message.payload);
        if (result) {
          waiter.resolve(result);
          this.waiters.splice(this.waiters.indexOf(waiter), 1);
        }
      }
    });
    await new Promise((resolve, reject) => {
      this.socket.once("open", resolve);
      this.socket.once("error", reject);
    });
  }

  send(value) {
    this.socket.send(JSON.stringify(value));
  }

  waitFor(predicate, timeoutMs = 10_000) {
    const current = this.latest && predicate(this.latest);
    if (current) return Promise.resolve(current);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        const index = this.waiters.indexOf(waiter);
        if (index >= 0) this.waiters.splice(index, 1);
        reject(new Error(`simulator snapshot timed out: ${JSON.stringify(this.latest)}`));
      }, timeoutMs);
      timer.unref();
      const waiter = {
        predicate,
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
      };
      this.waiters.push(waiter);
    });
  }

  close() {
    this.socket?.close();
  }
}

async function waitFor(action, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const result = await action();
      if (result) return result;
    } catch (error) {
      lastError = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw lastError ?? new Error("condition timed out");
}

function waitForLine(stream, pattern, timeoutMs = 10_000) {
  return new Promise((resolve, reject) => {
    let buffer = "";
    const timer = setTimeout(() => reject(new Error("process output timed out")), timeoutMs);
    stream.setEncoding("utf8");
    stream.on("data", (chunk) => {
      buffer += chunk;
      for (const line of buffer.split("\n")) {
        const match = line.match(pattern);
        if (match) {
          clearTimeout(timer);
          resolve(match[1]);
        }
      }
    });
  });
}

function waitForExit(child, timeoutMs) {
  return Promise.race([
    new Promise((resolve) => child.once("exit", resolve)),
    new Promise((_, reject) => setTimeout(() => reject(new Error("process exit timed out")), timeoutMs)),
  ]);
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return;
  child.kill("SIGTERM");
  try {
    await waitForExit(child, 3_000);
  } catch {
    child.kill("SIGKILL");
    await waitForExit(child, 3_000).catch(() => {});
  }
}

function captureProcessOutput(child) {
  let output = "";
  for (const stream of [child.stdout, child.stderr]) {
    stream?.setEncoding("utf8");
    stream?.on("data", (chunk) => {
      output = `${output}${chunk}`.slice(-16_384);
    });
  }
  return () => output;
}

await main();
