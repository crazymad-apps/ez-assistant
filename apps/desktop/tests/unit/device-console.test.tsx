import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import * as nativeCore from "@tauri-apps/api/core";
import type { DeviceGatewaySnapshot, SpeechConfigurationSnapshot } from "@ez-assistant/protocol";
import { AppMessageProvider } from "../../src/components/AppMessage";
import { DeviceDetailPage } from "../../src/features/settings/SettingsDialog/DeviceDetailPage";
import { DeviceSettingsPage } from "../../src/features/settings/SettingsDialog/DeviceSettingsPage";
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
  it("edits ASR and TTS speech settings with presets, voice picker, and live tests without select or DeepSeek UI", async () => {
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
    expect(screen.queryByText(/deepseek/i)).toBeNull();
    expect(container.querySelectorAll('[data-provider-icon="bailian"]')).toHaveLength(2);

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
});
