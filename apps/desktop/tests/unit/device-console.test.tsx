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

  let revCounter = 1;
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
    asr_profiles: [
      {
        id: "asr-default",
        name: "阿里云百炼识别",
        provider: "dashscope",
        endpoint: "https://dashscope.aliyuncs.com",
        model: "fun-asr-realtime",
        timeout_ms: 30000,
        credential_configured: true,
        credential_masked: "sk-***abcd",
      },
    ],
    active_asr_id: "asr-default",
    tts_profiles: [
      {
        id: "tts-default",
        name: "阿里云百炼女声",
        provider: "dashscope",
        endpoint: "https://dashscope.aliyuncs.com",
        model: "cosyvoice-v3-flash",
        voice: "longxiaochun",
        timeout_ms: 30000,
        credential_configured: true,
        credential_masked: "sk-***abcd",
      },
    ],
    active_tts_id: "tts-default",
    status: { asr: "ready", tts: "ready" },
  };

  const deviceGatewayCommand = vi.fn(async (command: { type: string; payload: unknown }) => {
    if (command.type === "get_snapshot") {
      return { type: "get_snapshot", payload: snapshot };
    }
    if (command.type === "get_speech_configuration") {
      return { type: "get_speech_configuration", payload: speechConfig };
    }
    if (command.type === "save_asr_profile") {
      const req = command.payload as {
        id?: string;
        name: string;
        provider: SpeechConfigurationSnapshot["asr"]["provider"];
        endpoint: string;
        model: string;
        timeout_ms: number;
        activate: boolean;
      };
      revCounter += 1;
      const id = req.id ?? `asr-${revCounter}`;
      const nextProfile = {
        id,
        name: req.name || req.model,
        provider: req.provider,
        endpoint: req.endpoint,
        model: req.model,
        timeout_ms: req.timeout_ms,
        credential_configured: true,
        credential_masked: "sk-***next",
      };
      const nextProfiles = req.id
        ? speechConfig.asr_profiles.map((item) => (item.id === id ? nextProfile : item))
        : [...speechConfig.asr_profiles, nextProfile];
      const activeId = req.activate ? id : speechConfig.active_asr_id;
      speechConfig = {
        ...speechConfig,
        revision: `rev-${revCounter}`,
        asr_profiles: nextProfiles,
        active_asr_id: activeId,
      };
      return {
        type: "save_asr_profile",
        payload: { configuration: speechConfig, snapshot },
      };
    }
    if (command.type === "save_tts_profile") {
      const req = command.payload as {
        id?: string;
        name: string;
        provider: SpeechConfigurationSnapshot["tts"]["provider"];
        endpoint: string;
        model: string;
        voice: string;
        timeout_ms: number;
        activate: boolean;
      };
      revCounter += 1;
      const id = req.id ?? `tts-${revCounter}`;
      const nextProfile = {
        id,
        name: req.name || req.voice,
        provider: req.provider,
        endpoint: req.endpoint,
        model: req.model,
        voice: req.voice,
        timeout_ms: req.timeout_ms,
        credential_configured: true,
        credential_masked: "sk-***next",
      };
      const nextProfiles = req.id
        ? speechConfig.tts_profiles.map((item) => (item.id === id ? nextProfile : item))
        : [...speechConfig.tts_profiles, nextProfile];
      const activeId = req.activate ? id : speechConfig.active_tts_id;
      speechConfig = {
        ...speechConfig,
        revision: `rev-${revCounter}`,
        tts_profiles: nextProfiles,
        active_tts_id: activeId,
      };
      return {
        type: "save_tts_profile",
        payload: { configuration: speechConfig, snapshot },
      };
    }
    if (command.type === "set_active_speech_profile") {
      const req = command.payload as { capability: "asr" | "tts"; profile_id?: string };
      revCounter += 1;
      speechConfig = {
        ...speechConfig,
        revision: `rev-${revCounter}`,
        active_asr_id: req.capability === "asr" ? req.profile_id : speechConfig.active_asr_id,
        active_tts_id: req.capability === "tts" ? req.profile_id : speechConfig.active_tts_id,
      };
      return {
        type: "set_active_speech_profile",
        payload: { configuration: speechConfig, snapshot },
      };
    }
    if (command.type === "delete_speech_profile") {
      const req = command.payload as { capability: "asr" | "tts"; profile_id: string };
      revCounter += 1;
      if (req.capability === "asr") {
        speechConfig = {
          ...speechConfig,
          revision: `rev-${revCounter}`,
          asr_profiles: speechConfig.asr_profiles.filter((item) => item.id !== req.profile_id),
          active_asr_id: speechConfig.active_asr_id === req.profile_id ? undefined : speechConfig.active_asr_id,
        };
      } else {
        speechConfig = {
          ...speechConfig,
          revision: `rev-${revCounter}`,
          tts_profiles: speechConfig.tts_profiles.filter((item) => item.id !== req.profile_id),
          active_tts_id: speechConfig.active_tts_id === req.profile_id ? undefined : speechConfig.active_tts_id,
        };
      }
      return {
        type: "delete_speech_profile",
        payload: { configuration: speechConfig, snapshot },
      };
    }
    if (command.type === "update_speech_configuration") {
      const req = command.payload as {
        asr: { provider: SpeechConfigurationSnapshot["asr"]["provider"]; endpoint: string; model: string; enabled: boolean; timeout_ms: number };
        tts: { provider: SpeechConfigurationSnapshot["tts"]["provider"]; endpoint: string; model: string; voice: string; enabled: boolean; timeout_ms: number };
      };
      revCounter += 1;
      speechConfig = {
        ...speechConfig,
        revision: `rev-${revCounter}`,
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
  it("manages saved ASR and TTS configuration lists with add, edit, switch, live tests, and delete without select", async () => {
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

    // 初始已保存列表展示，且无原生 <select>
    await screen.findByText("阿里云百炼识别");
    await screen.findByText("阿里云百炼女声");
    expect(container.querySelector("select")).toBeNull();
    expect(container.querySelectorAll('[data-provider-icon="bailian"]')).toHaveLength(2);
    expect(screen.getAllByText("当前使用")).toHaveLength(2);

    // 直接从列表试听已保存 TTS，并验证传入 profile_id
    fireEvent.click(screen.getByRole("button", { name: "试听 TTS 阿里云百炼女声" }));
    await screen.findByText(/合成成功 · 42 ms/);
    expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "test_tts_configuration",
        payload: expect.objectContaining({ profile_id: "tts-default" }),
      }),
    );

    // 直接从列表展开已保存 ASR 快速闭环测试
    fireEvent.click(screen.getByRole("button", { name: "测试 ASR 阿里云百炼识别" }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 闭环测试" }));
    await screen.findByText(/闭环识别测试通过/);
    expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "test_asr_configuration",
        payload: expect.objectContaining({ profile_id: "asr-default" }),
      }),
    );

    // 新增一个 OpenAI ASR 配置（验证 Fireworks 预置图标、OpenAI 高级设置与保存）
    fireEvent.click(screen.getByRole("button", { name: "新增语音识别配置" }));
    fireEvent.click(screen.getByRole("button", { name: "ASR 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /Fireworks AI/ }));
    expect(container.querySelector('[data-provider-icon="fireworks"]')).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "ASR 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /^OpenAI/ }));
    fireEvent.change(screen.getByLabelText("ASR 配置名称"), { target: { value: "OpenAI 主力识别" } });
    fireEvent.change(screen.getByLabelText("ASR API Key"), { target: { value: "sk-openai-asr" } });

    fireEvent.click(screen.getByRole("button", { name: "高级设置" }));
    await waitFor(() => {
      expect(screen.getByLabelText("ASR 接口地址")).toHaveValue("https://api.openai.com/v1");
      expect(screen.getByLabelText("ASR 模型名称")).toHaveValue("gpt-4o-mini-transcribe");
    });

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await screen.findByText("OpenAI 主力识别");
    expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "save_asr_profile",
        payload: expect.objectContaining({
          name: "OpenAI 主力识别",
          provider: "openai",
          endpoint: "https://api.openai.com/v1",
          model: "gpt-4o-mini-transcribe",
        }),
      }),
    );

    // 切换启用刚新增的 ASR 配置
    fireEvent.click(screen.getByRole("button", { name: "启用 ASR OpenAI 主力识别" }));
    await waitFor(() => {
      expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "set_active_speech_profile",
          payload: expect.objectContaining({
            capability: "asr",
            profile_id: "asr-2",
          }),
        }),
      );
    });

    // 新增硅基流动 TTS 配置（验证 DeepSeek / 阶跃星辰 / 硅基流动预置及音色选择）
    fireEvent.click(screen.getByRole("button", { name: "新增语音合成配置" }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /^DeepSeek/ }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 预置音色" }));
    fireEvent.click(await screen.findByRole("option", { name: /Tide · 沉稳男声/ }));

    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /阶跃星辰/ }));
    expect(container.querySelector('[data-provider-icon="stepfun"]')).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "TTS 服务提供商" }));
    fireEvent.click(await screen.findByRole("option", { name: /硅基流动/ }));
    fireEvent.click(screen.getByRole("button", { name: "TTS 预置音色" }));
    fireEvent.click(await screen.findByRole("option", { name: /Anna/ }));
    fireEvent.change(screen.getByLabelText("TTS API Key"), { target: { value: "sk-sf-tts" } });

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await screen.findByText("硅基流动 · anna");
    expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
      expect.objectContaining({
        type: "save_tts_profile",
        payload: expect.objectContaining({
          provider: "siliconflow",
          endpoint: "https://api.siliconflow.cn/v1",
          model: "FunAudioLLM/CosyVoice2-0.5B",
          voice: "anna",
        }),
      }),
    );

    // 停用 ASR 总开关时不删除列表配置
    fireEvent.click(screen.getByLabelText("启用语音识别"));
    await waitFor(() => {
      expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "set_active_speech_profile",
          payload: expect.objectContaining({
            capability: "asr",
            profile_id: undefined,
          }),
        }),
      );
    });
    expect(screen.getByText("OpenAI 主力识别")).toBeVisible();

    // 删除指定已保存配置（经过确认弹窗）
    fireEvent.click(screen.getByRole("button", { name: "删除 ASR OpenAI 主力识别" }));
    await screen.findByText("删除语音识别配置？");
    const dialogButtons = screen.getAllByRole("button", { name: "删除" });
    fireEvent.click(dialogButtons[dialogButtons.length - 1]);
    await waitFor(() => {
      expect(mock.deviceGatewayCommand).toHaveBeenCalledWith(
        expect.objectContaining({
          type: "delete_speech_profile",
          payload: expect.objectContaining({
            capability: "asr",
            profile_id: "asr-2",
          }),
        }),
      );
      expect(screen.queryByRole("button", { name: "删除 ASR OpenAI 主力识别" })).toBeNull();
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
              speech_end_silence_ms: 500,
              turn_commit_delay_ms: 0,
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

    await screen.findByText(/语音唤醒 · Mosaico-5BF0/);
    expect(container.querySelector("select")).toBeNull();
    expect(screen.getByLabelText("唤醒词")).toHaveValue("你好小元");

    // Trying voiceprint_direct with 0 speakers shows guardrail alert
    fireEvent.click(screen.getByRole("radio", { name: "声纹免唤醒" }));
    await screen.findByText(/请先录入声纹后再开启声纹唤醒/);

    // Edit wake word to 小元同学 -> auto pinyin xiao yuan tong xue + configure end-silence & commit delay -> save
    fireEvent.change(screen.getByLabelText("唤醒词"), { target: { value: "小元同学" } });
    fireEvent.click(screen.getByRole("button", { name: "微调拼音" }));
    expect(screen.getByLabelText("唤醒词拼音")).toHaveValue("xiao yuan tong xue");
    expect(screen.getByLabelText("说完判停延迟（毫秒）")).toHaveValue(500);
    expect(screen.getByLabelText("识别后入会话延迟（毫秒）")).toHaveValue(0);
    fireEvent.click(screen.getByRole("button", { name: "极速 0.35s" }));
    fireEvent.change(screen.getByLabelText("识别后入会话延迟（毫秒）"), { target: { value: "300" } });
    fireEvent.click(screen.getByRole("button", { name: "保存唤醒设置" }));

    await waitFor(() => {
      expect(savedWakeCalls).toHaveLength(1);
      expect(savedWakeCalls[0]).toEqual(
        expect.objectContaining({
          mode: "wake_word",
          wake_word: "小元同学",
          wake_pinyin: "xiao yuan tong xue",
          speech_end_silence_ms: 350,
          turn_commit_delay_ms: 300,
        }),
      );
    });

    // Click "录入声纹" jumps straight to VoiceprintTab with Mosaico capture
    fireEvent.click(screen.getByRole("button", { name: "录入声纹" }));
    await screen.findByText(/Mosaico-5BF0 本机录制/);
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
