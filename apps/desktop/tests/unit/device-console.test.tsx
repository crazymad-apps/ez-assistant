import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as nativeCore from "@tauri-apps/api/core";
import type { DeviceGatewaySnapshot, SpeechConfigurationSnapshot } from "@ez-assistant/protocol";
import { AppMessageProvider } from "../../src/components/AppMessage";
import { DeviceDetailPage } from "../../src/features/settings/SettingsDialog/DeviceDetailPage";
import { DeviceSettingsPage } from "../../src/features/settings/SettingsDialog/DeviceSettingsPage";
import {
  chineseWakeWordToPinyin,
  validateWakePinyin,
} from "../../src/features/settings/SettingsDialog/DeviceConsole/wakePinyin";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { RootStore } from "../../src/stores/RootStore";
import { RootStoreProvider } from "../../src/stores/RootStoreContext";

vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => undefined) }));

vi.mock("@tauri-apps/api/core", async () => ({
  ...(await vi.importActual<typeof import("@tauri-apps/api/core")>("@tauri-apps/api/core")),
  isTauri: vi.fn(() => false),
}));

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.mocked(nativeCore.isTauri).mockReturnValue(false);
});

function createConsoleMockClient() {
  const snapshot: DeviceGatewaySnapshot = {
    enabled: true,
    available: true,
    installation_id: "inst-1",
    certificate_fingerprint: "fp-1",
    pending_pairings: [],
    discovered_devices: [],
    devices: [
      {
        device_id: "gw-1",
        display_name: "客厅 ESP-Pika 网关",
        lifecycle: "paired",
        paired_at_ms: 1000,
        updated_at_ms: 2000,
        revoked_at_ms: null,
        connection: {
          connected_at_ms: 3000,
          capabilities: {
            input_text: true,
            input_pcm16_16k_mono: true,
            output_text: true,
            output_pcm16_16k_mono: true,
            playback_cancel: true,
            display_status: true,
            display_transcript: true,
          },
          output_preference: "text",
          peer_address: "192.168.1.33",
          client_version: "esp-pika-gateway/0.4.0",
          console: {
            api_prefix: "/api/v1",
            mode: "proxy",
            ui_path: "/ui/",
            direct_url: "http://192.168.1.33:8000/ui/",
          },
          speech_relay: true,
        },
      },
    ],
    speech_services: { asr: "ready", tts: "ready" },
  };

  let speechConfig: SpeechConfigurationSnapshot = {
    revision: "rev-1",
    asr: {
      enabled: true,
      provider: "dashscope",
      endpoint: "https://dashscope.aliyuncs.com",
      model: "fun-asr-realtime",
      timeout_ms: 30000,
      credential_configured: true,
      credential_masked: "sk-***abcd",
    },
    tts: {
      enabled: true,
      provider: "dashscope",
      endpoint: "https://dashscope.aliyuncs.com",
      model: "cosyvoice-v3-flash",
      voice: "longxiaochun",
      timeout_ms: 30000,
      credential_configured: true,
      credential_masked: "sk-***abcd",
    },
    status: { asr: "ready", tts: "ready" },
  };

  const deviceGatewayCommand = vi.fn(async (command: { type: string; payload: unknown }) => {
    if (command.type === "get_snapshot") {
      return { type: "get_snapshot", payload: snapshot };
    }
    if (command.type === "get_speech_configuration") {
      return { type: "get_speech_configuration", payload: speechConfig };
    }
    if (command.type === "update_speech_configuration") {
      const req = command.payload as {
        asr: { provider: SpeechConfigurationSnapshot["asr"]["provider"]; endpoint: string; model: string; enabled: boolean; timeout_ms: number };
        tts: { provider: SpeechConfigurationSnapshot["tts"]["provider"]; endpoint: string; model: string; voice: string; enabled: boolean; timeout_ms: number };
      };
      speechConfig = {
        revision: "rev-2",
        asr: {
          ...req.asr,
          credential_configured: true,
          credential_masked: "sk-***next",
        },
        tts: {
          ...req.tts,
          credential_configured: true,
          credential_masked: "sk-***next",
        },
        status: { asr: "ready", tts: "ready" },
      };
      return {
        type: "update_speech_configuration",
        payload: { configuration: speechConfig, snapshot },
      };
    }
    if (command.type === "test_tts_configuration") {
      return {
        type: "test_tts_configuration",
        payload: {
          audio_wav_base64: "UklGRg==",
          elapsed_ms: 42,
          duration_ms: 1500,
        },
      };
    }
    if (command.type === "test_asr_configuration") {
      return {
        type: "test_asr_configuration",
        payload: {
          transcript: "闭环识别测试通过",
          elapsed_ms: 35,
        },
      };
    }
    return { type: command.type, payload: { snapshot } };
  });

  const deviceConsoleJson = vi.fn(async (_deviceId: string, path: string, init?: { method?: string }) => {
    if (path === "api/v1/devices") {
      return {
        devices: [
          {
            hello: {
              device_id: "30:ed:a0:f3:dc:70",
              zone: "living_room",
              group_id: "home-default",
              chip: "esp32s3",
              vp_version: 2,
              capabilities: [{ name: "camera.capture" }],
            },
            shadow: { online: true, vad_state: "idle", rssi: -45, volume: 70 },
          },
        ],
      };
    }
    if (path === "api/v1/devices/30%3Aed%3Aa0%3Af3%3Adc%3A70/commands") {
      return { ok: true, mediaUrl: "/media/30ed/snap.jpg" };
    }
    if (path === "api/v1/macs" && (!init?.method || init.method === "GET")) {
      return { items: [{ mac: "30:ed:a0:f3:dc:70", group_id: "home-default", zone: "living_room", label: "客厅主机", enabled: true }] };
    }
    if (path === "api/v1/acls" && (!init?.method || init.method === "GET")) {
      return { rules: [{ rule_id: "r1", agent_id: "ez-assistant", group_id: "*", zone_pattern: "*", capability_pattern: "*", min_speaker_role: 1, allow: true }] };
    }
    if (path === "api/v1/speakers" && (!init?.method || init.method === "GET")) {
      return { speakers: [{ speaker_id: "spk-1", display_name: "主人", uid: "u1", agent_id: "ez-assistant", group_id: "home-default", role: 1, tts_voice: "longxiaochun", version: 1 }] };
    }
    if (path === "api/v1/voice/identify") {
      return { speaker_id: "spk-1", confidence: 0.91 };
    }
    if (path === "api/v1/voice/transcribe") {
      return { text: "测试识别结果" };
    }
    return { ok: true };
  });

  const deviceConsoleBlob = vi.fn(async () => new Blob([new Uint8Array([1, 2, 3])], { type: "image/jpeg" }));

  return {
    snapshot,
    deviceGatewayCommand,
    deviceConsoleJson,
    deviceConsoleBlob,
    client: {
      deviceGatewayCommand,
      deviceConsoleJson,
      deviceConsoleBlob,
    } as unknown as RuntimeClient,
  };
}

function createStoreWithClient(client: RuntimeClient): RootStore {
  const store = new RootStore();
  (store.device_gateway as unknown as { dependencies: { get_client: () => RuntimeClient | null; refresh_application: () => Promise<void> } }).dependencies = {
    get_client: () => client,
    refresh_application: async () => undefined,
  };
  return store;
}

describe("DeviceSpeechSection and DeviceConsole UI", () => {
  it("edits ASR and TTS speech settings with presets including DeepSeek, voice picker, and live tests without select", async () => {
    const mock = createConsoleMockClient();
    const store = createStoreWithClient(mock.client);
    vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:tts-preview");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const { container } = render(
      <RootStoreProvider store={store}>
        <AppMessageProvider>
          <DeviceSettingsPage />
        </AppMessageProvider>
      </RootStoreProvider>,
    );

    await screen.findByText(/仅需填写阿里云百炼 API Key/);
    expect(container.querySelector("select")).toBeNull();
    expect(container.querySelectorAll('[data-provider-icon="bailian"]')).toHaveLength(2);

    // 切换 ASR 与 TTS 为 DeepSeek 验证官方语音预置、图标、双端复用提示与 Tide 音色选择
    fireEvent.click(screen.getByRole("button", { name: "ASR 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /^DeepSeek/ }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /^DeepSeek/ }));
    expect(container.querySelectorAll('[data-provider-icon="deepseek"]')).toHaveLength(2);
    await screen.findByText(/仅需填写 DeepSeek API Key（sk-...）/);
    fireEvent.click(screen.getByRole("button", { name: "TTS 预置音色" }));
    fireEvent.click(await screen.findByRole("option", { name: /Tide · 沉稳男声/ }));

    // 切换 TTS 为阶跃星辰验证新预置与音色直选、再切回硅基流动试听闭环测试
    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /阶跃星辰/ }));
    expect(container.querySelector('[data-provider-icon="stepfun"]')).not.toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "TTS 预置音色" }));
    fireEvent.click(await screen.findByRole("option", { name: /温柔女声/ }));

    // 切换 TTS 为硅基流动并挑选预置音色、试听合成音频，再用该音频闭环测试 ASR
    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /硅基流动/ }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 预置音色" }));
    fireEvent.click(await screen.findByRole("option", { name: /Anna/ }));

    fireEvent.click(screen.getByRole("button", { name: "试听音色" }));
    await screen.findByText(/合成成功 · 耗时 42 ms/);

    fireEvent.click(screen.getByRole("button", { name: "用 TTS 音频闭环测试" }));
    await screen.findByText(/闭环识别测试通过/);

    // 切换 ASR 为 Fireworks 验证多 ASR 预置，再切换 OpenAI 展开高级设置
    fireEvent.click(screen.getByRole("button", { name: "ASR 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /Fireworks AI/ }));
    expect(container.querySelector('[data-provider-icon="fireworks"]')).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "ASR 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /^OpenAI/ }));

    const advancedButtons = screen.getAllByRole("button", { name: "高级设置" });
    fireEvent.click(advancedButtons[0]);

    await waitFor(() => {
      expect(screen.getByLabelText("ASR 接口地址")).toHaveValue("https://api.openai.com/v1");
      expect(screen.getByLabelText("ASR 模型名称")).toHaveValue("gpt-4o-mini-transcribe");
    });

    fireEvent.click(screen.getByRole("button", { name: "保存并应用" }));

    await waitFor(() => {
      expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "update_speech_configuration",
          payload: expect.objectContaining({
            expected_revision: "rev-1",
            asr: expect.objectContaining({
              provider: "openai",
              endpoint: "https://api.openai.com/v1",
              model: "gpt-4o-mini-transcribe",
            }),
            tts: expect.objectContaining({
              provider: "siliconflow",
              endpoint: "https://api.siliconflow.cn/v1",
              model: "FunAudioLLM/CosyVoice2-0.5B",
              voice: "anna",
              reuse_asr_credential: false,
            }),
          }),
        }),
      );
    });
    store.dispose();
  });

  it("renders controlled console tabs when console metadata exists and omits them otherwise", async () => {
    const mock = createConsoleMockClient();
    const store = createStoreWithClient(mock.client);
    const createObjectUrlSpy = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:mock-snap");
    vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);

    const { container } = render(
      <RootStoreProvider store={store}>
        <AppMessageProvider>
          <DeviceDetailPage device={mock.snapshot.devices[0]} on_back={() => undefined} />
        </AppMessageProvider>
      </RootStoreProvider>,
    );

    expect(screen.getByText("http://192.168.1.33:8000/ui/")).toBeVisible();
    expect(screen.getByText("已开启")).toBeVisible();
    expect(screen.queryByText(/deepseek/i)).toBeNull();

    fireEvent.click(screen.getByRole("tab", { name: "设备拓扑与指令" }));
    await screen.findAllByText(/living_room \(30:ed:a0:f3:dc:70\)/);
    fireEvent.click(screen.getByRole("button", { name: "发送指令" }));

    await waitFor(() => {
      expect(mock.deviceConsoleBlob).toHaveBeenCalledWith("gw-1", "media/30ed/snap.jpg");
      expect(createObjectUrlSpy).toHaveBeenCalled();
      expect(screen.getByAltText("终端抓拍画面")).toHaveAttribute("src", "blob:mock-snap");
    });

    fireEvent.click(screen.getByRole("tab", { name: "MAC 白名单" }));
    await screen.findByText("客厅主机");

    fireEvent.click(screen.getByRole("tab", { name: "ACL 权限矩阵" }));
    await screen.findByText("r1");

    fireEvent.click(screen.getByRole("tab", { name: "声纹与语音" }));
    await screen.findByText("主人");

    fireEvent.click(screen.getByRole("button", { name: "生成合成样本 150Hz" }));
    fireEvent.change(screen.getByLabelText("说话人 ID"), { target: { value: "spk-new" } });
    fireEvent.click(screen.getByRole("button", { name: "声纹注册 (Enroll)" }));

    await waitFor(() => {
      expect(mock.deviceConsoleJson).toHaveBeenCalledWith(
        "gw-1",
        "api/v1/voice/enroll",
        expect.objectContaining({ method: "POST" }),
      );
    });

    fireEvent.click(screen.getByRole("button", { name: "声纹识别验证 (Identify)" }));
    await screen.findByText(/spk-1 \(本地高可信\)/);

    fireEvent.click(screen.getByRole("button", { name: "ASR 转写测试 (Transcribe)" }));
    await screen.findByText(/测试识别结果/);

    expect(container.querySelector("select")).toBeNull();
    expect(screen.queryByText(/deepseek/i)).toBeNull();

    cleanup();
    const deviceWithoutConsole = {
      ...mock.snapshot.devices[0],
      connection: {
        ...mock.snapshot.devices[0].connection!,
        console: undefined,
      },
    };
    render(
      <RootStoreProvider store={store}>
        <AppMessageProvider>
          <DeviceDetailPage device={deviceWithoutConsole} on_back={() => undefined} />
        </AppMessageProvider>
      </RootStoreProvider>,
    );
    expect(screen.queryByRole("tab", { name: "声纹与语音" })).toBeNull();
    store.dispose();
  });

  it("renders native direct-connect console tabs without MAC whitelist or external console URL", async () => {
    const mock = createConsoleMockClient();
    const store = createStoreWithClient(mock.client);
    const nativeDevice = {
      ...mock.snapshot.devices[0],
      device_id: "mosaico-1",
      display_name: "Mosaico-5BF0",
      connection: {
        ...mock.snapshot.devices[0].connection!,
        client_version: "0.4.0-direct",
        console: {
          api_prefix: "/api/v1",
          mode: "native" as const,
        },
        speech_relay: false,
      },
    };
    render(
      <RootStoreProvider store={store}>
        <AppMessageProvider>
          <DeviceDetailPage device={nativeDevice} on_back={() => undefined} />
        </AppMessageProvider>
      </RootStoreProvider>,
    );

    expect(screen.getByRole("tab", { name: "概览" })).toBeVisible();
    expect(screen.getByRole("tab", { name: "设备控制与状态" })).toBeVisible();
    expect(screen.getByRole("tab", { name: "ACL 权限矩阵" })).toBeVisible();
    expect(screen.getByRole("tab", { name: "声纹与语音" })).toBeVisible();
    expect(screen.queryByRole("tab", { name: "MAC 白名单" })).toBeNull();
    expect(screen.queryByText("原始控制台")).toBeNull();

    fireEvent.click(screen.getByRole("tab", { name: "设备控制与状态" }));
    await screen.findByText(/直连硬件状态与实时遥测/);
    store.dispose();
  });

  it("converts Chinese wake words to MultiNet7 pinyin, enforces voiceprint guardrail, and syncs wake settings on DeviceSettingsPage", async () => {
    expect(chineseWakeWordToPinyin("你好小元")).toEqual({
      pinyin: "ni hao xiao yuan",
      unknown_chars: [],
    });
    expect(chineseWakeWordToPinyin("绿茶")).toEqual({
      pinyin: "lv cha",
      unknown_chars: [],
    });
    expect(validateWakePinyin("ni hao xiao yuan").valid).toBe(true);
    expect(validateWakePinyin("ni hao xyz").valid).toBe(false);

    const mock = createConsoleMockClient();
    mock.snapshot.devices[0] = {
      ...mock.snapshot.devices[0],
      device_id: "mosaico-1",
      display_name: "Mosaico-5BF0",
      connection: {
        ...mock.snapshot.devices[0].connection!,
        client_version: "0.5.0-direct",
        console: {
          api_prefix: "/api/v1",
          mode: "native",
        },
        speech_relay: false,
      },
    };
    let enrolledCount = 0;
    const savedWakeCalls: unknown[] = [];
    const customConsoleJson = vi.fn(
      async (_deviceId: string, path: string, init?: { method?: string; body?: unknown }): Promise<unknown> => {
        if (path === "api/v1/wake" && (!init?.method || init.method === "GET")) {
          return {
            wake: {
              mode: "wake_word",
              wake_word: "你好小元",
              wake_pinyin: "ni hao xiao yuan",
              vp_threshold: 0.78,
              min_speech_ms: 700,
              cooldown_ms: 1500,
              vp_wake_chime: false,
            },
            enrolled_speaker_count: enrolledCount,
            multinet_ready: true,
          };
        }
        if (path === "api/v1/wake" && init?.method === "PUT") {
          savedWakeCalls.push(init.body);
          return { ok: true, synced: true, wake: init.body, enrolled_speaker_count: enrolledCount };
        }
        if (path === "api/v1/speakers") {
          return { speakers: [] };
        }
        if (path === "api/v1/voice/capture_embedding") {
          return {
            ok: true,
            embedding: Array.from({ length: 192 }, (_, idx) => (idx === 0 ? 1 : 0)),
            voiced_ms: 1400,
            snr_db: 17.5,
            rms: 0.12,
            duration_ms: 2100,
          };
        }
        if (path === "api/v1/voice/enroll") {
          enrolledCount = 1;
          return { ok: true, speaker_id: "spk-luke", template_count: 1, synced_devices: 1 };
        }
        return { ok: true };
      },
    );
    mock.client.deviceConsoleJson = customConsoleJson as unknown as RuntimeClient["deviceConsoleJson"];

    const store = createStoreWithClient(mock.client);
    const { container } = render(
      <RootStoreProvider store={store}>
        <AppMessageProvider>
          <DeviceSettingsPage />
        </AppMessageProvider>
      </RootStoreProvider>,
    );

    await screen.findByText(/语音唤醒与模式 · Mosaico-5BF0/);
    expect(container.querySelector("select")).toBeNull();
    expect(screen.getByLabelText("唤醒词")).toHaveValue("你好小元");

    // Trying voiceprint_direct with 0 speakers shows guardrail alert
    fireEvent.click(screen.getByRole("radio", { name: /声纹免唤醒/ }));
    await screen.findByText(/请先为至少 1 位成员录入声纹/);

    // Edit wake word to 小元同学 -> auto pinyin xiao yuan tong xue -> save
    fireEvent.change(screen.getByLabelText("唤醒词"), { target: { value: "小元同学" } });
    expect(screen.getByText("xiao yuan tong xue")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "保存唤醒设置" }));

    await waitFor(() => {
      expect(savedWakeCalls).toHaveLength(1);
      expect(savedWakeCalls[0]).toEqual(
        expect.objectContaining({
          mode: "wake_word",
          wake_word: "小元同学",
          wake_pinyin: "xiao yuan tong xue",
        }),
      );
    });

    // Click "立即录入声纹" jumps straight to VoiceprintTab with Mosaico capture
    fireEvent.click(screen.getByRole("button", { name: "立即录入声纹" }));
    await screen.findByText(/用 Mosaico-5BF0 本机麦克风录制/);
    fireEvent.click(screen.getByRole("button", { name: "用 Mosaico 录第 1 句" }));
    await screen.findByText(/第 1\/3 句采集完成/);

    fireEvent.change(screen.getByLabelText("说话人 ID"), { target: { value: "spk-luke" } });
    fireEvent.click(screen.getByRole("button", { name: "声纹注册 (Enroll)" }));
    await waitFor(() => {
      expect(customConsoleJson).toHaveBeenCalledWith(
        "mosaico-1",
        "api/v1/voice/enroll",
        expect.objectContaining({
          method: "POST",
          body: expect.objectContaining({
            speaker_id: "spk-luke",
            embeddings: [expect.any(Array)],
          }),
        }),
      );
    });
    store.dispose();
  });
});
