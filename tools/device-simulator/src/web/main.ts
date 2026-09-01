import "./styles.css";
import { BrowserPcmRecorder } from "./pcmRecorder";
import {
  BrowserPcmPlayback,
  type PlaybackEnd,
  type PlaybackStart,
} from "./pcmPlayback";

interface Host {
  installationId: string;
  certificateFingerprint: string;
  pairingAvailable: boolean;
}

interface Snapshot {
  phase: string;
  hosts: Host[];
  pairingCode?: string;
  pairedDeviceId?: string;
  outputPreference: string;
  terminalProfile: "voice_only" | "screen_voice" | "keyboard_screen" | "mixed";
  declaredCapabilities: DeviceCapabilities;
  capabilities: DeviceCapabilities;
  armedFault?: string;
  currentInteraction?: {
    clientInputId: string;
    inputId?: string;
    runId?: string;
    queueState?: string;
    textOutput?: string;
  };
  currentPlayback?: {
    text: string;
    status: string;
    receivedBytes: number;
  };
  lastError?: string;
  diagnostics: Array<{ at: string; kind: string; detail: string }>;
}

interface DeviceCapabilities {
  input_text: boolean;
  input_pcm16_16k_mono: boolean;
  output_text: boolean;
  output_pcm16_16k_mono: boolean;
  playback_cancel: boolean;
  display_status: boolean;
  display_transcript: boolean;
}

const token = document.querySelector<HTMLMetaElement>('meta[name="simulator-token"]')?.content;
if (!token) throw new Error("simulator page token is missing");
const socket = new WebSocket(
  `ws://${location.host}/bridge?token=${encodeURIComponent(token)}`,
);
socket.binaryType = "arraybuffer";

const hosts = required<HTMLSelectElement>("hosts");
const phase = required<HTMLElement>("phase");
const deviceId = required<HTMLElement>("device-id");
const pairingCode = required<HTMLElement>("pairing-code");
const preference = required<HTMLSelectElement>("preference");
const terminalProfile = required<HTMLSelectElement>("terminal-profile");
const error = required<HTMLElement>("error");
const events = required<HTMLOListElement>("events");
const armedFault = required<HTMLElement>("armed-fault");
const fault = required<HTMLSelectElement>("fault");
const textInput = required<HTMLTextAreaElement>("text-input");
const submitText = required<HTMLButtonElement>("submit-text");
const retryText = required<HTMLButtonElement>("retry-text");
const clientInputId = required<HTMLElement>("client-input-id");
const runtimeInputId = required<HTMLElement>("runtime-input-id");
const runId = required<HTMLElement>("run-id");
const queueState = required<HTMLElement>("queue-state");
const textOutput = required<HTMLElement>("text-output");
const startRecording = required<HTMLButtonElement>("start-recording");
const stopRecording = required<HTMLButtonElement>("stop-recording");
const submitTestPcm = required<HTMLButtonElement>("submit-test-pcm");
const retryPcm = required<HTMLButtonElement>("retry-pcm");
const recordingState = required<HTMLElement>("recording-state");
const enablePlayback = required<HTMLButtonElement>("enable-playback");
const cancelPlayback = required<HTMLButtonElement>("cancel-playback");
const playbackState = required<HTMLElement>("playback-state");
const recorder = new BrowserPcmRecorder();
const player = new BrowserPcmPlayback((status) => {
  playbackState.textContent = status.text;
  playbackState.dataset.active = String(status.active);
  enablePlayback.textContent = status.enabled ? "播报已启用" : "启用播报";
  enablePlayback.disabled = status.enabled;
  cancelPlayback.disabled = !status.active;
  if (status.text.startsWith("playback_")) error.textContent = status.text;
});
let isStoppingRecording = false;

socket.addEventListener("message", (event) => {
  if (event.data instanceof ArrayBuffer) {
    player.frame(new Uint8Array(event.data));
    return;
  }
  const message: unknown = JSON.parse(String(event.data));
  if (!isRecord(message)) return;
  if (message.type === "snapshot") render(message.payload as Snapshot);
  if (message.type === "playback_start" && isPlaybackStart(message.payload)) {
    player.start(message.payload);
  }
  if (message.type === "playback_end" && isPlaybackEnd(message.payload)) {
    player.end(message.payload);
  }
  if (message.type === "action_error" && typeof message.message === "string") {
    error.textContent = message.message;
  }
});

required<HTMLButtonElement>("connect").addEventListener("click", () => {
  send({ type: "connect", hostInstallationId: hosts.value });
});
required<HTMLButtonElement>("disconnect").addEventListener("click", () => {
  send({ type: "disconnect" });
});
required<HTMLButtonElement>("reset").addEventListener("click", () => {
  if (window.confirm("确定清除模拟设备的本地配对凭据吗？")) {
    send({ type: "reset_device" });
  }
});
submitText.addEventListener("click", () => {
  send({ type: "submit_text", text: textInput.value });
});
retryText.addEventListener("click", () => {
  send({ type: "retry_text" });
});
submitTestPcm.addEventListener("click", () => {
  send({ type: "submit_test_pcm" });
});
retryPcm.addEventListener("click", () => {
  send({ type: "retry_pcm" });
});
startRecording.addEventListener("click", () => {
  void beginRecording();
});
stopRecording.addEventListener("click", () => {
  void finishRecording(false);
});
enablePlayback.addEventListener("click", () => {
  void enableBrowserPlayback();
});
cancelPlayback.addEventListener("click", () => {
  send({ type: "cancel_playback" });
  player.stopLocal();
});
preference.addEventListener("change", () => {
  send({ type: "set_output_preference", preference: preference.value });
});
terminalProfile.addEventListener("change", () => {
  send({ type: "set_terminal_profile", profile: terminalProfile.value });
});
required<HTMLButtonElement>("inject-fault").addEventListener("click", () => {
  send({ type: "inject_fault", fault: fault.value });
});
window.addEventListener("beforeunload", () => {
  void recorder.cancel();
  void player.close();
});

async function beginRecording(): Promise<void> {
  error.textContent = "";
  startRecording.disabled = true;
  try {
    await recorder.start(() => void finishRecording(true));
    stopRecording.disabled = false;
    recordingState.textContent = "录音中 · 最长 60 秒";
    recordingState.dataset.active = "true";
  } catch (recordingError) {
    startRecording.disabled = false;
    error.textContent = errorMessage(recordingError);
  }
}

async function enableBrowserPlayback(): Promise<void> {
  try {
    await player.enable();
    if (preference.value === "text") {
      preference.value = "text_and_audio";
      send({ type: "set_output_preference", preference: preference.value });
      playbackState.textContent = "播报已启用 · 输出已切换为文字 + 语音";
    }
  } catch (playbackError) {
    error.textContent = errorMessage(playbackError);
  }
}

async function finishRecording(reachedLimit: boolean): Promise<void> {
  if (!recorder.isRecording || isStoppingRecording) return;
  isStoppingRecording = true;
  stopRecording.disabled = true;
  recordingState.textContent = "正在转换并发送";
  try {
    const result = await recorder.stop();
    sendPcm(result.pcm);
    recordingState.textContent = `${reachedLimit ? "已到 60 秒上限" : "已发送"} · ${(
      result.durationMs / 1_000
    ).toFixed(1)} 秒`;
  } catch (recordingError) {
    recordingState.textContent = "录音失败";
    error.textContent = errorMessage(recordingError);
  } finally {
    delete recordingState.dataset.active;
    startRecording.disabled = false;
    isStoppingRecording = false;
  }
}

function render(snapshot: Snapshot): void {
  phase.textContent = snapshot.phase;
  phase.dataset.phase = snapshot.phase;
  deviceId.textContent = snapshot.pairedDeviceId ?? "未配对";
  pairingCode.textContent = snapshot.pairingCode ?? "—";
  preference.value = snapshot.outputPreference;
  terminalProfile.value = snapshot.terminalProfile;
  terminalProfile.disabled = !isDisconnectedPhase(snapshot.phase);
  renderCapabilities(snapshot.capabilities);
  error.textContent = snapshot.lastError ?? "";
  armedFault.textContent = snapshot.armedFault ? `已就绪：${snapshot.armedFault}` : "";
  clientInputId.textContent = snapshot.currentInteraction?.clientInputId ?? "—";
  runtimeInputId.textContent = snapshot.currentInteraction?.inputId ?? "—";
  runId.textContent = snapshot.currentInteraction?.runId ?? "—";
  queueState.textContent = snapshot.currentInteraction?.queueState ?? "—";
  textOutput.textContent = snapshot.currentInteraction?.textOutput
    ?? snapshot.currentPlayback?.text
    ?? "尚无终端回复";
  const selected = hosts.value;
  hosts.replaceChildren(...snapshot.hosts.map((host) => {
    const option = document.createElement("option");
    option.value = host.installationId;
    option.textContent = `${host.installationId.slice(0, 8)} · ${host.pairingAvailable ? "可配对" : "已发现"}`;
    option.title = host.certificateFingerprint;
    return option;
  }));
  if (snapshot.hosts.some((host) => host.installationId === selected)) hosts.value = selected;
  events.replaceChildren(...snapshot.diagnostics.slice(-100).reverse().map((entry) => {
    const item = document.createElement("li");
    const time = new Date(entry.at).toLocaleTimeString();
    item.textContent = `${time}  ${entry.kind} · ${entry.detail}`;
    return item;
  }));
}

function renderCapabilities(capabilities: DeviceCapabilities): void {
  textInput.disabled = !capabilities.input_text;
  submitText.disabled = !capabilities.input_text;
  retryText.disabled = !capabilities.input_text;
  if (!recorder.isRecording) startRecording.disabled = !capabilities.input_pcm16_16k_mono;
  submitTestPcm.disabled = !capabilities.input_pcm16_16k_mono;
  retryPcm.disabled = !capabilities.input_pcm16_16k_mono;
  enablePlayback.disabled = player.isEnabled || !capabilities.output_pcm16_16k_mono;
  cancelPlayback.disabled = !player.isActive || !capabilities.playback_cancel;
  for (const option of preference.options) {
    option.disabled = option.value === "text"
      ? !capabilities.output_text
      : option.value === "audio"
        ? !capabilities.output_pcm16_16k_mono
        : !capabilities.output_text || !capabilities.output_pcm16_16k_mono;
  }
}

function isDisconnectedPhase(value: string): boolean {
  return value === "discovering" || value === "disconnected";
}

function send(value: unknown): void {
  error.textContent = "";
  if (socket.readyState !== WebSocket.OPEN) throw new Error("H5 与 Node 尚未连接");
  socket.send(JSON.stringify(value));
}

function sendPcm(pcm: Uint8Array): void {
  error.textContent = "";
  if (socket.readyState !== WebSocket.OPEN) throw new Error("H5 与 Node 尚未连接");
  const payload = new ArrayBuffer(pcm.byteLength);
  new Uint8Array(payload).set(pcm);
  socket.send(payload);
}

function required<T extends HTMLElement>(id: string): T {
  const element = document.getElementById(id);
  if (!element) throw new Error(`#${id} is missing`);
  return element as T;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function isPlaybackStart(value: unknown): value is PlaybackStart {
  return isRecord(value)
    && typeof value.outputId === "string"
    && typeof value.runId === "string"
    && Number.isInteger(value.streamId)
    && typeof value.streamId === "number"
    && typeof value.text === "string"
    && Number.isInteger(value.sampleCount)
    && typeof value.sampleCount === "number";
}

function isPlaybackEnd(value: unknown): value is PlaybackEnd {
  return isRecord(value)
    && typeof value.outputId === "string"
    && Number.isInteger(value.streamId)
    && typeof value.streamId === "number"
    && typeof value.reason === "string";
}

function errorMessage(value: unknown): string {
  return value instanceof Error ? value.message : String(value);
}
