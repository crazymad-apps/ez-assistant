import { afterEach, describe, expect, it, vi } from "vitest";
import type { DeviceGatewaySnapshot, SpeechConfigurationSnapshot } from "@ez-assistant/protocol";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { DeviceGatewayStore } from "../../src/stores/DeviceGatewayStore";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("DeviceGatewayStore", () => {
  it("keeps Host snapshots authoritative and refreshes Runtime hosting after device mutations", async () => {
    const snapshot = gatewaySnapshot();
    const device_command = vi.fn(async (command: { type: string }) => command.type === "get_snapshot"
      ? { type: "get_snapshot", payload: snapshot }
      : { type: command.type, payload: { snapshot } });
    const runtime_command = vi.fn(async () => ({
      type: "set_current_controller_output_hosting",
      payload: { changed: true, session: {} },
    }));
    const refresh_application = vi.fn(async () => undefined);
    const store = new DeviceGatewayStore({
      get_client: () => ({
        command: runtime_command,
        deviceGatewayCommand: device_command,
      }) as unknown as RuntimeClient,
      refresh_application,
    });

    await store.load();
    expect(store.snapshot).toEqual(snapshot);
    expect(store.stale).toBe(false);

    await store.renameDevice("device-1", "书房终端");
    expect(device_command).toHaveBeenCalledWith({
      type: "rename_device",
      payload: { device_id: "device-1", display_name: "书房终端" },
    });
    expect(refresh_application).toHaveBeenCalledTimes(1);

    await store.setOutputHosting("device-1");
    expect(runtime_command).toHaveBeenCalledWith({
      type: "set_current_controller_output_hosting",
      payload: { device_id: "device-1" },
    });
    expect(refresh_application).toHaveBeenCalledTimes(2);
  });

  it("validates pairing codes locally and coalesces invalidation refreshes", async () => {
    vi.useFakeTimers();
    const snapshot = gatewaySnapshot();
    const device_command = vi.fn(async (command: { type: string }) => command.type === "get_snapshot"
      ? { type: "get_snapshot", payload: snapshot }
      : { type: command.type, payload: { snapshot } });
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    expect(await store.confirmPairing("pairing-1", "123", null)).toBe(false);
    expect(device_command).not.toHaveBeenCalled();
    expect(store.error_message).toContain("6 位配对码");

    expect(await store.confirmPairing("pairing-direct", null, "书房终端")).toBe(true);
    expect(device_command).toHaveBeenCalledWith({
      type: "confirm_pairing",
      payload: { pairing_request_id: "pairing-direct", pairing_code: null, display_name: "书房终端" },
    });

    expect(await store.rejectPairing("pairing-direct")).toBe(true);
    expect(device_command).toHaveBeenCalledWith({
      type: "reject_pairing",
      payload: { pairing_request_id: "pairing-direct" },
    });

    device_command.mockClear();
    store.scheduleRefresh();
    store.scheduleRefresh();
    await vi.advanceTimersByTimeAsync(80);
    expect(device_command).toHaveBeenCalledTimes(1);
    expect(store.stale).toBe(false);
  });

  it("loads and updates Host speech configuration and projects updated gateway status", async () => {
    const initialConfig = speechConfigSnapshot();
    const updatedSnapshot: DeviceGatewaySnapshot = {
      ...gatewaySnapshot(),
      speech_services: { asr: "ready", tts: "ready" },
    };
    const updatedConfig: SpeechConfigurationSnapshot = {
      ...initialConfig,
      revision: "rev-2",
      status: { asr: "ready", tts: "ready" },
    };
    const device_command = vi.fn(async (command: { type: string }) => {
      if (command.type === "get_speech_configuration") {
        return { type: "get_speech_configuration", payload: initialConfig };
      }
      return {
        type: "update_speech_configuration",
        payload: { configuration: updatedConfig, snapshot: updatedSnapshot },
      };
    });
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    await store.loadSpeechConfiguration();
    expect(store.speech_config).toEqual(initialConfig);

    const saved = await store.updateSpeechConfiguration({
      expected_revision: "rev-1",
      asr: {
        enabled: true,
        provider: "openai",
        endpoint: "https://api.openai.com/v1",
        model: "gpt-4o-mini-transcribe",
        timeout_ms: 15000,
        credential: "sk-test",
        clear_credential: false,
      },
      tts: {
        enabled: true,
        provider: "openai",
        endpoint: "https://api.openai.com/v1",
        model: "gpt-4o-mini-tts",
        voice: "alloy",
        timeout_ms: 15000,
        credential: null,
        clear_credential: false,
        reuse_asr_credential: false,
      },
    });
    expect(saved).toBe(true);
    expect(store.speech_config).toEqual(updatedConfig);
    expect(store.snapshot?.speech_services).toEqual({ asr: "ready", tts: "ready" });
    expect(store.notice_message).toContain("保存");
  });

  it("invokes testAsrConfiguration and testTtsConfiguration via DeviceGatewayCommand", async () => {
    const device_command = vi.fn(async (command: { type: string }) => {
      if (command.type === "test_asr_configuration") {
        return {
          type: "test_asr_configuration",
          payload: { transcript: "测试识别结果", elapsed_ms: 120 },
        };
      }
      return {
        type: "test_tts_configuration",
        payload: { audio_wav_base64: "UklGRg==", elapsed_ms: 85, duration_ms: 1200 },
      };
    });
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    const asrRes = await store.testAsrConfiguration({
      asr: {
        enabled: false,
        provider: "groq",
        endpoint: "https://api.groq.com/openai/v1",
        model: "whisper-large-v3-turbo",
        timeout_ms: 30000,
        credential: "gsk-test",
        clear_credential: false,
      },
      audio_wav_base64: "UklGRg==",
    });
    expect(asrRes.transcript).toBe("测试识别结果");

    const ttsRes = await store.testTtsConfiguration({
      tts: {
        enabled: false,
        provider: "minimax",
        endpoint: "https://api.minimaxi.com/v1/t2a_v2",
        model: "speech-2.8-turbo",
        voice: "female-shaonv",
        timeout_ms: 30000,
        credential: null,
        clear_credential: false,
        reuse_asr_credential: true,
      },
      fallback_asr_credential: null,
      text: "你好",
    });
    expect(ttsRes.duration_ms).toBe(1200);
    expect(ttsRes.elapsed_ms).toBe(85);
  });
});

function speechConfigSnapshot(): SpeechConfigurationSnapshot {
  return {
    revision: "rev-1",
    asr: {
      enabled: true,
      provider: "dashscope",
      endpoint: "https://dashscope.aliyuncs.com",
      model: "fun-asr-realtime",
      timeout_ms: 30000,
      credential_configured: true,
      credential_masked: "sk-***1234",
    },
    tts: {
      enabled: true,
      provider: "dashscope",
      endpoint: "https://dashscope.aliyuncs.com",
      model: "cosyvoice-v3-flash",
      voice: "longxiaochun",
      timeout_ms: 30000,
      credential_configured: true,
      credential_masked: "sk-***1234",
    },
    status: { asr: "ready", tts: "ready" },
  };
}

function gatewaySnapshot(): DeviceGatewaySnapshot {
  return {
    enabled: true,
    available: true,
    installation_id: "installation-1",
    certificate_fingerprint: "fingerprint",
    pairing_window: { expires_at_ms: 1_000 },
    pending_pairings: [],
    devices: [{
      device_id: "device-1",
      display_name: "客厅终端",
      lifecycle: "paired",
      paired_at_ms: 1,
      updated_at_ms: 1,
      revoked_at_ms: null,
      connection: {
        connected_at_ms: 2,
        output_preference: "text",
        capabilities: {
          input_text: true,
          input_pcm16_16k_mono: false,
          output_text: true,
          output_pcm16_16k_mono: false,
          playback_cancel: false,
          display_status: true,
          display_transcript: true,
        },
      },
    }],
    speech_services: { asr: "unavailable", tts: "unavailable" },
  };
}
