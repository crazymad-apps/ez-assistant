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

  it("saves, activates, and deletes saved ASR and TTS profiles", async () => {
    const initialConfig = speechConfigSnapshot();
    const afterSave: SpeechConfigurationSnapshot = {
      ...initialConfig,
      revision: "rev-2",
      asr_profiles: [
        ...initialConfig.asr_profiles,
        {
          id: "asr-2",
          name: "DeepSeek 备用",
          provider: "deepseek",
          endpoint: "https://api.deepseek.com/v1",
          model: "deepseek-asr",
          timeout_ms: 25000,
          credential_configured: true,
          credential_masked: "sk-****9999",
        },
      ],
      active_asr_id: "asr-2",
    };
    const afterDisable: SpeechConfigurationSnapshot = {
      ...afterSave,
      revision: "rev-3",
      active_asr_id: undefined,
      asr: { ...afterSave.asr, enabled: false },
    };
    const afterDelete: SpeechConfigurationSnapshot = {
      ...afterDisable,
      revision: "rev-4",
      asr_profiles: initialConfig.asr_profiles,
    };
    const device_command = vi.fn(async (command: { type: string }) => {
      if (command.type === "get_speech_configuration") {
        return { type: "get_speech_configuration", payload: initialConfig };
      }
      if (command.type === "save_asr_profile") {
        return { type: "save_asr_profile", payload: { configuration: afterSave, snapshot: gatewaySnapshot() } };
      }
      if (command.type === "set_active_speech_profile") {
        return { type: "set_active_speech_profile", payload: { configuration: afterDisable, snapshot: gatewaySnapshot() } };
      }
      if (command.type === "delete_speech_profile") {
        return { type: "delete_speech_profile", payload: { configuration: afterDelete, snapshot: gatewaySnapshot() } };
      }
      return { type: command.type, payload: { snapshot: gatewaySnapshot() } };
    });
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    await store.loadSpeechConfiguration();
    expect(await store.saveAsrProfile({
      expected_revision: "rev-1",
      name: "DeepSeek 备用",
      provider: "deepseek",
      endpoint: "https://api.deepseek.com/v1",
      model: "deepseek-asr",
      timeout_ms: 25000,
      credential: "sk-deepseek-9999",
      clear_credential: false,
      activate: true,
    })).toBe(true);
    expect(store.speech_config?.asr_profiles).toHaveLength(2);
    expect(store.speech_config?.active_asr_id).toBe("asr-2");

    expect(await store.setActiveSpeechProfile("asr", null)).toBe(true);
    expect(device_command).toHaveBeenLastCalledWith({
      type: "set_active_speech_profile",
      payload: { expected_revision: "rev-2", capability: "asr", profile_id: undefined },
    });
    expect(store.speech_config?.active_asr_id).toBeUndefined();

    expect(await store.deleteSpeechProfile("asr", "asr-2")).toBe(true);
    expect(device_command).toHaveBeenLastCalledWith({
      type: "delete_speech_profile",
      payload: { expected_revision: "rev-3", capability: "asr", profile_id: "asr-2" },
    });
    expect(store.speech_config?.asr_profiles).toHaveLength(1);
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

  it("connects a discovered local device via connect_discovered_device command", async () => {
    const discoveredSnapshot: DeviceGatewaySnapshot = {
      ...gatewaySnapshot(),
      discovered_devices: [
        {
          discovery_id: "Mosaico-5BF0._ez-device._tcp.local.",
          display_name: "Mosaico-5BF0",
          model: "esp32s31-mosaico",
          peer_address: "192.168.110.88",
          control_port: 8765,
          key_fingerprint: "1A2B-3C4D",
        },
      ],
    };
    const device_command = vi.fn(async (command: { type: string }) => command.type === "get_snapshot"
      ? { type: "get_snapshot", payload: discoveredSnapshot }
      : { type: "connect_discovered_device", payload: { snapshot: gatewaySnapshot() } });
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    await store.load();
    expect(store.snapshot?.discovered_devices).toHaveLength(1);

    const ok = await store.connectDiscoveredDevice("Mosaico-5BF0._ez-device._tcp.local.", "  书房 Mosaico  ");
    expect(ok).toBe(true);
    expect(device_command).toHaveBeenCalledWith({
      type: "connect_discovered_device",
      payload: {
        discovery_id: "Mosaico-5BF0._ez-device._tcp.local.",
        display_name: "书房 Mosaico",
      },
    });
    expect(store.snapshot?.discovered_devices).toEqual([]);
    expect(store.notice_message).toContain("正在连接设备");
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
    asr_profiles: [
      {
        id: "asr-default",
        name: "阿里云百炼 · fun-asr-realtime",
        provider: "dashscope",
        endpoint: "https://dashscope.aliyuncs.com",
        model: "fun-asr-realtime",
        timeout_ms: 30000,
        credential_configured: true,
        credential_masked: "sk-***1234",
      },
    ],
    active_asr_id: "asr-default",
    tts_profiles: [
      {
        id: "tts-default",
        name: "阿里云百炼 · longxiaochun",
        provider: "dashscope",
        endpoint: "https://dashscope.aliyuncs.com",
        model: "cosyvoice-v3-flash",
        voice: "longxiaochun",
        timeout_ms: 30000,
        credential_configured: true,
        credential_masked: "sk-***1234",
      },
    ],
    active_tts_id: "tts-default",
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
    discovered_devices: [],
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
