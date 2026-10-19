import { expect, it, vi } from "vitest";
import { DesktopController } from "../../src/features/resource-workspace/DesktopController";
import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { DesktopEvent } from "../../src/runtime-client/DesktopSocket";

function fixture() {
  const callbacks: Array<(event: DesktopEvent) => void> = [];
  const sockets: Array<{ disconnect: ReturnType<typeof vi.fn>; decoded: ReturnType<typeof vi.fn> }> = [];
  const client = { openDesktop: vi.fn((_device: string, receive: (event: DesktopEvent) => void) => {
    callbacks.push(receive);
    const socket = { disconnect: vi.fn(), decoded: vi.fn() };
    sockets.push(socket);
    return socket;
  }) } as unknown as RuntimeClient;
  return { client, callbacks, sockets };
}

it("解码与 Host 就绪分别确认，重连后忽略旧连接事件", () => {
  const f = fixture();
  const controller = new DesktopController("target", "测试电脑", () => f.client);
  const stop = vi.fn();
  f.callbacks[0]!({ type: "stream", stream: { getTracks: () => [{ stop }] } as unknown as MediaStream });
  expect(controller.status).toBe("connecting");
  controller.decoded();
  expect(f.sockets[0]!.decoded).toHaveBeenCalledOnce();
  f.callbacks[0]!({ type: "ready" });
  expect(controller.status).toBe("viewing");
  controller.connect();
  expect(stop).toHaveBeenCalledOnce();
  expect(f.sockets[0]!.disconnect).toHaveBeenCalledOnce();
  f.callbacks[0]!({ type: "error", message: "旧连接" });
  expect(controller.status).toBe("connecting");
  expect(controller.error).toBeNull();
  controller.disconnect();
  expect(f.sockets[1]!.disconnect).toHaveBeenCalledOnce();
});

it("同组重复打开聚焦已有桌面，关闭和账号断开立即清理；M1 不保存活动连接", () => {
  const f = fixture();
  const store = new ResourceWorkspaceStore(() => f.client);
  store.openDesktop("target", "测试电脑");
  store.openDesktop("target", "测试电脑");
  expect(f.client.openDesktop).toHaveBeenCalledOnce();
  expect(store.tabs.filter((tab) => tab.type === "desktop")).toHaveLength(1);
  expect(store.captureSnapshot().groups[0]!.tabs).toHaveLength(1);
  store.closeTab(store.active_tab_key);
  expect(f.sockets[0]!.disconnect).toHaveBeenCalledOnce();
  expect(store.desktops.size).toBe(0);
  store.openDesktop("other", "另一个设备");
  store.disconnectTerminals();
  expect(f.sockets[1]!.disconnect).toHaveBeenCalledOnce();
  store.dispose();
});


it("同设备不同会话和草稿共享一个桌面页面，删除原会话不结束桌面", () => {
  const f = fixture();
  const store = new ResourceWorkspaceStore(() => f.client);
  store.selectScope("session:first");
  store.openDesktop("target", "测试电脑", true);
  const key = store.active_tab_key;
  const page = store.mounted_pages.find((item) => item.tab.type === "desktop")!;
  const controller = [...store.desktops.values()][0]!;
  expect(controller.can_control).toBe(true);
  store.selectScope("session:second");
  expect(store.active_tab_key).toBe(key);
  expect(store.mounted_pages.filter((item) => item.tab.type === "desktop").map((item) => item.key)).toEqual([page.key]);
  store.activateTab("context");
  store.openDesktop("target", "测试电脑", true);
  expect(f.client.openDesktop).toHaveBeenCalledOnce();
  store.releaseScope("session:first");
  expect(f.sockets[0]!.disconnect).not.toHaveBeenCalled();
  store.selectScope("draft:unbound");
  expect(store.active_tab_key).toBe(key);
  store.releaseScope("draft:unbound");
  expect(store.desktops.size).toBe(1);
  store.openDesktop("target", "测试电脑", true);
  store.closeTab(key);
  expect(f.sockets[0]!.disconnect).toHaveBeenCalledOnce();
  store.selectScope("session:second");
  expect(store.active_tab.type).toBe("context");
  store.dispose();
});

it("切屏结束旧控制和画面，新访问首帧后仍只读，迟到回调不覆盖新屏", () => {
  const f = fixture();
  const controller = new DesktopController("target", "测试电脑", () => f.client, true);
  const displays = [{ id: "primary", width: 1920, height: 1080, primary: true }, { id: "portrait", width: 1080, height: 1920, primary: false }];
  f.callbacks[0]!({ type: "displays", displays, selected: "primary" });
  f.callbacks[0]!({ type: "ready" });
  f.callbacks[0]!({ type: "control_ready" });
  controller.selectDisplay("portrait");
  expect(f.sockets[0]!.disconnect).toHaveBeenCalledOnce();
  expect(f.client.openDesktop).toHaveBeenLastCalledWith("target", expect.any(Function), "portrait");
  expect(controller.control).toBe("readonly");
  f.callbacks[0]!({ type: "control_ready" });
  f.callbacks[0]!({ type: "displays", displays, selected: "primary" });
  expect(controller.display_id).toBe("portrait");
  expect(controller.control).toBe("readonly");
  f.callbacks[1]!({ type: "ready" });
  expect(controller.status).toBe("viewing");
  expect(controller.control).toBe("readonly");
  controller.disconnect();
});

it("恢复只保存设备与显示器，保持断开，控制权限使用当前目录", async () => {
  const f = fixture();
  const original = new ResourceWorkspaceStore(() => f.client);
  original.openDesktop("target", "测试电脑", true);
  f.callbacks[0]!({ type: "displays", displays: [{ id: "screen-b", width: 1920, height: 1440, primary: true }], selected: "screen-b" });
  const snapshot = original.captureSnapshot();
  expect(snapshot.desktops).toEqual([{ device: "target", name: "测试电脑", display_id: "screen-b" }]);
  original.dispose();
  const restored = new ResourceWorkspaceStore(() => f.client, (id) => id === "target");
  await restored.restoreSnapshot(snapshot);
  expect(f.client.openDesktop).toHaveBeenCalledOnce();
  const desktop = [...restored.desktops.values()][0]!;
  expect(desktop.status).toBe("disconnected");
  expect(desktop.control).toBe("readonly");
  expect(desktop.can_control).toBe(true);
  expect(restored.active_tab.type).toBe("desktop");
  desktop.connect();
  expect(f.client.openDesktop).toHaveBeenLastCalledWith("target", expect.any(Function), "screen-b");
  restored.dispose();
});
