import { render } from "../support/renderWithAppearance";
import { fireEvent, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { DesktopUpdatePanel, DesktopUpdateStore } from "../../src/features/desktop-update";
import type { DesktopUpdateSnapshot } from "../../src/native-bridge/desktopUpdate";

const bridge = vi.hoisted(() => ({ read: vi.fn(), listen: vi.fn(), start: vi.fn(), cancel: vi.fn(), unlisten: vi.fn() }));
vi.mock("../../src/native-bridge/desktopUpdate", () => ({ readDesktopUpdate: bridge.read, listenDesktopUpdate: bridge.listen, startDesktopUpdate: bridge.start, cancelDesktopUpdate: bridge.cancel }));
const state = (patch: Partial<DesktopUpdateSnapshot> = {}): DesktopUpdateSnapshot => ({ revision: 1, phase: "idle", available: false, required: false, notes: "", received: 0, total: 0, error: null, resume_remote: null, stops_local_host: false, binding_id: null, host_mode: null, ...patch });

beforeEach(() => {
  vi.clearAllMocks();
  bridge.listen.mockResolvedValue(bridge.unlisten);
  bridge.read.mockResolvedValue(state());
  bridge.start.mockResolvedValue(undefined);
  bridge.cancel.mockResolvedValue(undefined);
});

describe("Desktop update projection", () => {
  it("ignores a late read after a newer native event and keeps the operation when closing the panel", async () => {
    const store = new DesktopUpdateStore(true);
    bridge.listen.mockImplementation(async (receive: (snapshot: DesktopUpdateSnapshot) => void) => {
      receive(state({ revision: 2, phase: "downloading", available: true, total: 100, received: 40 }));
      return bridge.unlisten;
    });
    await store.initialize();
    expect(store.snapshot.received).toBe(40);
    expect(store.visible).toBe(true);
    store.show(); store.hide(); store.show();
    expect(store.snapshot.phase).toBe("downloading");
    expect(bridge.start).not.toHaveBeenCalled();
    expect(bridge.cancel).not.toHaveBeenCalled();
    store.dispose();
    expect(bridge.unlisten).toHaveBeenCalledOnce();
  });

  it("hides idle and unreachable updates while retaining an ongoing operation", () => {
    const store = new DesktopUpdateStore(true);
    expect(store.visible).toBe(false);
    store.receive(state({ required: true }));
    expect(store.visible).toBe(false);
    store.receive(state({ revision: 2, available: true, phase: "available" }));
    expect(store.visible).toBe(true);
    store.receive(state({ revision: 3 }));
    expect(store.visible).toBe(false);
    store.dispose();
  });

  it("shows one update action, allows download cancellation, and prevents dismissing installation", async () => {
    const store = new DesktopUpdateStore(true);
    store.receive(state({ available: true, phase: "available", notes: "修复启动问题。" }));
    store.show();
    const view = render(<DesktopUpdatePanel store={store} />);
    expect(screen.queryByText("检查更新")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "更新并重启" }));
    expect(bridge.start).toHaveBeenCalledOnce();
    store.receive(state({ revision: 2, available: true, phase: "downloading", total: 100, received: 20 }));
    view.rerender(<DesktopUpdatePanel store={store} />);
    fireEvent.click(screen.getByRole("button", { name: "取消下载" }));
    expect(bridge.cancel).toHaveBeenCalledOnce();
    store.receive(state({ revision: 3, available: true, phase: "installing" }));
    view.rerender(<DesktopUpdatePanel store={store} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(store.open).toBe(true);
    expect(screen.queryByRole("button", { name: "取消下载" })).not.toBeInTheDocument();
    view.unmount(); store.dispose();
  });
});
