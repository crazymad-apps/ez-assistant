import { beforeEach, describe, expect, it, vi } from "vitest";
import type { HostRegistrationStatus } from "@ez-assistant/protocol";
import { HostRegistrationStore } from "../../src/features/settings/SettingsDialog/HostRegistrationPage/store";
import { manageHostRegistration, observeHostRegistration } from "../../src/native-bridge/hostRegistration";

vi.mock("../../src/native-bridge/hostRegistration", () => ({
  manageHostRegistration: vi.fn(),
  localComputerName: vi.fn().mockResolvedValue("办公电脑"),
  observeHostRegistration: vi.fn().mockReturnValue(vi.fn()),
}));
const status: HostRegistrationStatus = {
  center_id: null,
  installation_id: null,
  registration_id: null,
  intent: null,
  device: null,
  error: null,
  connection: "disconnected",
};
const flush = async () => {
  await Promise.resolve();
  await Promise.resolve();
};

beforeEach(() => vi.clearAllMocks());

describe("本机登记交互边界", () => {
  it("结果未知时不重发申请，查询原状态后才解锁", async () => {
    vi.mocked(manageHostRegistration).mockResolvedValue(status);
    const store = new HostRegistrationStore();
    store.start();
    await flush();
    vi.mocked(manageHostRegistration).mockRejectedValueOnce(new Error("请求超时"));
    await store.submit({ type: "apply", payload: { name: "办公电脑" } });
    expect(store.uncertain).toBe(true);
    const calls = vi.mocked(manageHostRegistration).mock.calls.length;
    await store.submit({ type: "apply", payload: { name: "另一台" } });
    expect(manageHostRegistration).toHaveBeenCalledTimes(calls);
    await store.refresh();
    expect(store.uncertain).toBe(false);
    store.dispose();
  });

  it("中心未知不显示成未登记，也不覆盖用户草稿", async () => {
    vi.mocked(manageHostRegistration).mockResolvedValue(status);
    const store = new HostRegistrationStore();
    store.start();
    await flush();
    store.setName("我填写的名称");
    await store.refresh();
    expect(store.name).toBe("我填写的名称");
    vi.mocked(manageHostRegistration).mockRejectedValueOnce(new Error("断网"));
    await store.refresh();
    expect(store.uncertain).toBe(true);
    expect(store.status).toEqual(status);
    store.dispose();
  });

  it("重新申请的名称草稿不会被旧登记刷新覆盖", async () => {
    const ended: HostRegistrationStatus = {
      ...status,
      device: {
        id: "old",
        installation_id: "installation",
        name: "旧名称",
        platform: "macos",
        host_version: "0.31.0",
        status: "revoked",
        enabled: true,
        online: false,
      },
    };
    vi.mocked(manageHostRegistration).mockResolvedValue(ended);
    const store = new HostRegistrationStore();
    store.start();
    await flush();
    store.setName("重新申请的名称");
    await store.refresh();
    expect(store.name).toBe("重新申请的名称");
    expect(store.initialName).toBe("旧名称");
    store.dispose();
  });

  it("卸载后丢弃迟到状态并停止订阅，不能恢复旧页面", async () => {
    let resolve!: (value: HostRegistrationStatus) => void;
    vi.mocked(manageHostRegistration).mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const stop = vi.fn();
    vi.mocked(observeHostRegistration).mockReturnValueOnce(stop);
    const store = new HostRegistrationStore();
    store.start();
    store.dispose();
    resolve(status);
    await flush();
    expect(store.status).toBeNull();
    expect(stop).toHaveBeenCalledOnce();
  });

  it("事件突发合并查询，提交期间的通知只在提交结束后核实", async () => {
    vi.mocked(manageHostRegistration).mockResolvedValue(status);
    const store = new HostRegistrationStore();
    store.start();
    await flush();
    let resolve!: (value: HostRegistrationStatus) => void;
    vi.mocked(manageHostRegistration).mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    const write = store.submit({ type: "exit" });
    await store.refresh();
    await store.refresh();
    const count = vi.mocked(manageHostRegistration).mock.calls.length;
    resolve({ ...status, intent: "closed" });
    await write;
    await flush();
    expect(manageHostRegistration).toHaveBeenCalledTimes(count + 1);
    store.dispose();
  });
});
