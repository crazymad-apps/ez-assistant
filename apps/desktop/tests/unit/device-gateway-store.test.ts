import { afterEach, describe, expect, it, vi } from "vitest";
import type { DeviceGatewaySnapshot } from "../../src/generated/assistant-protocol";
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
    const device_command = vi.fn(async () => ({ type: "get_snapshot", payload: gatewaySnapshot() }));
    const store = new DeviceGatewayStore({
      get_client: () => ({ deviceGatewayCommand: device_command }) as unknown as RuntimeClient,
      refresh_application: async () => undefined,
    });

    expect(await store.confirmPairing("pairing-1", "123", null)).toBe(false);
    expect(device_command).not.toHaveBeenCalled();
    expect(store.error_message).toContain("6 位配对码");

    store.scheduleRefresh();
    store.scheduleRefresh();
    await vi.advanceTimersByTimeAsync(80);
    expect(device_command).toHaveBeenCalledTimes(1);
    expect(store.stale).toBe(false);
  });
});

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
