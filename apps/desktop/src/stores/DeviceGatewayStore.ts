import { action, makeObservable, observable, observableRef, runInAction } from "mobx";
import type {
  DeviceGatewaySnapshot,
  DeviceId,
  SpeechConfigurationSnapshot,
  TestAsrConfigurationRequest,
  TestAsrConfigurationResult,
  TestTtsConfigurationRequest,
  TestTtsConfigurationResult,
  UpdateSpeechConfigurationRequest,
} from "@ez-assistant/protocol";
import type { RuntimeClient } from "../runtime-client/RuntimeClient";

type DeviceGatewayDependencies = Readonly<{
  get_client: () => RuntimeClient | null;
  refresh_application: () => Promise<void>;
}>;

/** Desktop 对 Host Device Gateway 权威快照的薄投影，不推导在线、过期或托管状态。 */
export class DeviceGatewayStore {
  snapshot: DeviceGatewaySnapshot | null = null;
  speech_config: SpeechConfigurationSnapshot | null = null;
  loading = false;
  speech_loading = false;
  stale = true;
  pending_action: string | null = null;
  error_message: string | null = null;
  notice_message: string | null = null;
  #request = 0;
  #speech_request = 0;
  #refresh_timer: number | null = null;

  constructor(private readonly dependencies: DeviceGatewayDependencies) {
    makeObservable(this, {
      snapshot: observableRef,
      speech_config: observableRef,
      loading: observable,
      speech_loading: observable,
      stale: observable,
      pending_action: observable,
      error_message: observable,
      notice_message: observable,
      load: action,
      loadSpeechConfiguration: action,
      updateSpeechConfiguration: action,
      testAsrConfiguration: action,
      testTtsConfiguration: action,
      scheduleRefresh: action,
      markStale: action,
      setAccessEnabled: action,
      openPairingWindow: action,
      closePairingWindow: action,
      confirmPairing: action,
      rejectPairing: action,
      connectDiscoveredDevice: action,
      renameDevice: action,
      revokeDevice: action,
      setOutputHosting: action,
      clearMessages: action,
      dispose: action,
    });
  }

  async load(silent = false): Promise<void> {
    const client = this.dependencies.get_client();
    if (!client) {
      this.stale = true;
      if (!silent) this.error_message = "运行时尚未连接。";
      return;
    }
    const request = ++this.#request;
    if (!silent) this.loading = true;
    try {
      const result = await client.deviceGatewayCommand({ type: "get_snapshot", payload: {} });
      if (result.type !== "get_snapshot") {
        throw new Error("Host 返回了不匹配的设备快照结果。");
      }
      runInAction(() => {
        if (request !== this.#request) return;
        this.snapshot = result.payload;
        this.stale = false;
        if (!silent) this.error_message = null;
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (request !== this.#request) return;
        this.stale = true;
        if (!silent) this.error_message = displayError(error);
      });
    } finally {
      runInAction(() => {
        if (request === this.#request) this.loading = false;
      });
    }
  }

  scheduleRefresh(): void {
    if (this.#refresh_timer !== null) window.clearTimeout(this.#refresh_timer);
    this.#refresh_timer = window.setTimeout(() => {
      this.#refresh_timer = null;
      void this.load(true);
    }, 80);
  }

  markStale(): void {
    this.stale = true;
  }

  async setAccessEnabled(enabled: boolean): Promise<boolean> {
    return this.#runGatewayAction(
      "access",
      { type: "set_access_enabled", payload: { enabled } },
      enabled ? "智能终端接入已启用。" : "智能终端接入已停用。",
    );
  }

  async openPairingWindow(): Promise<boolean> {
    return this.#runGatewayAction(
      "pairing:open",
      { type: "open_pairing_window", payload: {} },
      "已进入添加设备状态。",
    );
  }

  async closePairingWindow(): Promise<boolean> {
    return this.#runGatewayAction(
      "pairing:close",
      { type: "close_pairing_window", payload: {} },
      "已退出添加设备状态。",
    );
  }

  async confirmPairing(
    pairing_request_id: string,
    pairing_code: string | null,
    display_name: string | null,
  ): Promise<boolean> {
    if (pairing_code !== null && !/^\d{6}$/.test(pairing_code)) {
      this.error_message = "请输入终端显示或播报的 6 位配对码。";
      this.notice_message = null;
      return false;
    }
    return this.#runGatewayAction(
      `pairing:${pairing_request_id}`,
      {
        type: "confirm_pairing",
        payload: {
          pairing_request_id,
          pairing_code: pairing_code ?? null,
          display_name: display_name?.trim() || null,
        },
      },
      pairing_code === null
        ? "已发送配对确认，正在等待终端完成绑定。"
        : "配对码已提交，正在等待终端完成确认。",
    );
  }

  async rejectPairing(pairing_request_id: string): Promise<boolean> {
    return this.#runGatewayAction(
      `pairing:${pairing_request_id}`,
      {
        type: "reject_pairing",
        payload: { pairing_request_id },
      },
      "已拒绝该终端的配对请求。",
    );
  }

  async connectDiscoveredDevice(
    discovery_id: string,
    display_name?: string | null,
  ): Promise<boolean> {
    const trimmed_name = display_name?.trim();
    return this.#runGatewayAction(
      `connect:${discovery_id}`,
      {
        type: "connect_discovered_device",
        payload: trimmed_name ? { discovery_id, display_name: trimmed_name } : { discovery_id },
      },
      "正在连接设备，完成后将自动上线。",
    );
  }

  async renameDevice(device_id: DeviceId, display_name: string): Promise<boolean> {
    const next_name = display_name.trim();
    if (!next_name) {
      this.error_message = "设备名称不能为空。";
      this.notice_message = null;
      return false;
    }
    const saved = await this.#runGatewayAction(
      `rename:${device_id}`,
      { type: "rename_device", payload: { device_id, display_name: next_name } },
      "设备名称已更新。",
    );
    if (saved) await this.dependencies.refresh_application();
    return saved;
  }

  async revokeDevice(device_id: DeviceId): Promise<boolean> {
    const revoked = await this.#runGatewayAction(
      `revoke:${device_id}`,
      { type: "revoke_device", payload: { device_id } },
      "设备配对已解除。",
    );
    if (revoked) await this.dependencies.refresh_application();
    return revoked;
  }

  async setOutputHosting(device_id: DeviceId | null): Promise<boolean> {
    const client = this.dependencies.get_client();
    if (!client || this.pending_action) return false;
    this.pending_action = "hosting";
    this.clearMessages();
    try {
      await client.command({
        type: "set_current_controller_output_hosting",
        payload: { device_id },
      });
      await this.dependencies.refresh_application();
      runInAction(() => {
        this.notice_message = device_id ? "PC 输出托管目标已更新。" : "PC 输出托管已解除。";
      });
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_action = null;
      });
    }
  }

  consoleJson<T>(
    device_id: string,
    relative_path: string,
    init?: Readonly<{ method?: "GET" | "POST" | "PUT" | "DELETE"; body?: unknown; signal?: AbortSignal }>,
  ): Promise<T> {
    const client = this.dependencies.get_client();
    if (!client) {
      return Promise.reject(new Error("运行时尚未连接。"));
    }
    return client.deviceConsoleJson<T>(device_id, relative_path, init);
  }

  consoleBlob(
    device_id: string,
    relative_path: string,
    signal?: AbortSignal,
  ): Promise<Blob> {
    const client = this.dependencies.get_client();
    if (!client) {
      return Promise.reject(new Error("运行时尚未连接。"));
    }
    return signal
      ? client.deviceConsoleBlob(device_id, relative_path, signal)
      : client.deviceConsoleBlob(device_id, relative_path);
  }

  clearMessages(): void {
    this.error_message = null;
    this.notice_message = null;
  }

  dispose(): void {
    this.#request += 1;
    this.#speech_request += 1;
    if (this.#refresh_timer !== null) {
      window.clearTimeout(this.#refresh_timer);
      this.#refresh_timer = null;
    }
  }

  async loadSpeechConfiguration(silent = false): Promise<void> {
    const client = this.dependencies.get_client();
    if (!client) return;
    const request = ++this.#speech_request;
    if (!silent) this.speech_loading = true;
    try {
      const result = await client.deviceGatewayCommand({ type: "get_speech_configuration", payload: {} });
      if (result.type !== "get_speech_configuration") {
        throw new Error("Host 返回了不匹配的语音配置结果。");
      }
      runInAction(() => {
        if (request !== this.#speech_request) return;
        this.speech_config = result.payload;
        if (!silent) this.error_message = null;
      });
    } catch (error: unknown) {
      runInAction(() => {
        if (request !== this.#speech_request) return;
        if (!silent) this.error_message = displayError(error);
      });
    } finally {
      runInAction(() => {
        if (request === this.#speech_request) this.speech_loading = false;
      });
    }
  }

  async updateSpeechConfiguration(payload: UpdateSpeechConfigurationRequest): Promise<boolean> {
    const client = this.dependencies.get_client();
    if (!client || this.pending_action) return false;
    this.pending_action = "speech";
    this.clearMessages();
    try {
      const result = await client.deviceGatewayCommand({ type: "update_speech_configuration", payload });
      if (result.type !== "update_speech_configuration") {
        throw new Error("Host 返回了不匹配的语音更新结果。");
      }
      runInAction(() => {
        this.speech_config = result.payload.configuration;
        this.snapshot = result.payload.snapshot;
        this.stale = false;
        this.notice_message = "语音服务配置已保存并热加载。";
      });
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_action = null;
      });
    }
  }

  async testAsrConfiguration(payload: TestAsrConfigurationRequest): Promise<TestAsrConfigurationResult> {
    const client = this.dependencies.get_client();
    if (!client) {
      throw new Error("运行时尚未连接。");
    }
    const result = await client.deviceGatewayCommand({ type: "test_asr_configuration", payload });
    if (result.type !== "test_asr_configuration") {
      throw new Error("Host 返回了不匹配的语音识别测试结果。");
    }
    return result.payload;
  }

  async testTtsConfiguration(payload: TestTtsConfigurationRequest): Promise<TestTtsConfigurationResult> {
    const client = this.dependencies.get_client();
    if (!client) {
      throw new Error("运行时尚未连接。");
    }
    const result = await client.deviceGatewayCommand({ type: "test_tts_configuration", payload });
    if (result.type !== "test_tts_configuration") {
      throw new Error("Host 返回了不匹配的语音合成测试结果。");
    }
    return result.payload;
  }

  async #runGatewayAction<TType extends Exclude<
    import("@ez-assistant/protocol").DeviceGatewayCommand["type"],
    | "get_snapshot"
    | "get_speech_configuration"
    | "update_speech_configuration"
    | "test_asr_configuration"
    | "test_tts_configuration"
  >>(
    action_name: string,
    command: Extract<import("@ez-assistant/protocol").DeviceGatewayCommand, { type: TType }>,
    notice: string,
  ): Promise<boolean> {
    const client = this.dependencies.get_client();
    if (!client || this.pending_action) return false;
    this.pending_action = action_name;
    this.clearMessages();
    try {
      const result = await client.deviceGatewayCommand(command);
      if (
        result.type === "get_snapshot"
        || result.type === "get_speech_configuration"
        || result.type === "update_speech_configuration"
        || result.type === "test_asr_configuration"
        || result.type === "test_tts_configuration"
      ) {
        throw new Error("Host 返回了不匹配的设备 mutation 结果。");
      }
      runInAction(() => {
        this.snapshot = result.payload.snapshot;
        this.stale = false;
        this.notice_message = notice;
      });
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        this.error_message = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        this.pending_action = null;
      });
    }
  }
}

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法读取智能终端状态。";
}
