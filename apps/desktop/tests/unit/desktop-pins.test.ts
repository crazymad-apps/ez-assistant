import { expect, it, vi } from "vitest";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { SessionSummary } from "@ez-assistant/protocol";
import { DesktopPinsStore } from "../../src/stores/DesktopPinsStore";
import { parseDesktopWorkspace, type DesktopWorkspaceSnapshot } from "../../src/features/resource-workspace/desktopWorkspaceSnapshot";

const device = "11111111-1111-4111-8111-111111111111";
const pin = { device, session_id: "same-id", title: "同名会话", device_name: "工作设备" };
const session = (id: string, pinned = true, role = "normal") => ({ session_id: id, title: id, is_pinned: pinned, role }) as SessionSummary;

it("imports the Host pin projection independently of sidebar pages and persists the marker", async () => {
  const save = vi.fn().mockResolvedValue(undefined);
  const store = new DesktopPinsStore(save);
  const command = vi.fn().mockResolvedValue({ payload: { snapshot: { value: { pinned_sessions: [session("a"), session("b")] } } } });
  await store.importLegacy({ command } as unknown as RuntimeClient, true);
  expect(command).toHaveBeenCalledWith({ type: "get_application_snapshot", payload: { session_groups: [] } });
  expect(store.value.items.map((item) => item.session_id)).toEqual(["a", "b"]);
  expect(save).toHaveBeenCalledOnce();
  expect(save.mock.calls[0]![0]).toEqual({ imported: true, items: store.value.items });
  await store.importLegacy({ command } as unknown as RuntimeClient, true);
  expect(command).toHaveBeenCalledTimes(1);
});

it("does not read direct remote pins and preserves state when saving fails; retry is idempotent", async () => {
  const save = vi.fn().mockRejectedValueOnce(new Error("disk full")).mockResolvedValue(undefined);
  const store = new DesktopPinsStore(save);
  const command = vi.fn();
  await store.importLegacy({ command } as unknown as RuntimeClient, false);
  expect(command).not.toHaveBeenCalled();
  expect(store.value.imported).toBe(false);
  await store.importLegacy({ command } as unknown as RuntimeClient, false);
  expect(store.value.imported).toBe(true);
  save.mockRejectedValueOnce(new Error("readonly"));
  expect(await store.set(pin, true)).toBe(false);
  expect(store.has(pin.session_id, device)).toBe(false);
  expect(await store.set(pin, true)).toBe(true);
  await store.set({ ...pin, device: null }, true);
  await store.set(pin, false);
  expect(store.has(pin.session_id, null)).toBe(true);
  expect(store.has(pin.session_id, device)).toBe(false);
});

it("does not admit a delayed identity result after disposal", async () => {
  let resolve!: () => void;
  const store = new DesktopPinsStore(() => new Promise<void>((done) => { resolve = done; }));
  const pending = store.set(pin, true);
  store.dispose(); resolve();
  expect(await pending).toBe(false);
  expect(store.value.items).toEqual([]);
});

it("validates owner/device resource restoration and rejects foreign local handles", () => {
  const snapshot: DesktopWorkspaceSnapshot = { version: 1, owner: "identity-a", pins: { imported: true, items: [pin] },
    devices: [{ device, expanded: ["project-a"], resources: {
      current_scope_key: "session:session-a", groups: [{ scope_key: "session:session-a", active_index: 1, focused_index: 1,
        tabs: [{ page: { type: "context" } }, { page: { type: "workspace" } }] }],
    } }] };
  expect(parseDesktopWorkspace(snapshot, "identity-a")).toEqual(snapshot);
  expect(() => parseDesktopWorkspace(snapshot, "identity-b")).toThrow();
  const invalid = structuredClone(snapshot);
  Object.assign(invalid.devices[0]!.resources!.groups[0]!.tabs[1]!, { page: { type: "resource", name: "local", source: { type: "local_file", path_segments: ["/", "private", "file"] }, line: null } });
  expect(() => parseDesktopWorkspace(invalid, "identity-a")).toThrow();
  expect(parseDesktopWorkspace({ ...snapshot, selection: { device: null, scope: "session:session-a" } }, "identity-a")).toEqual(snapshot);
});

it("reorders cross-device offline pins locally and preserves saved order after failure", async () => {
  const save = vi.fn().mockResolvedValue(undefined);
  const store = new DesktopPinsStore(save);
  const local = { ...pin, device: null };
  const remote = { ...pin, device: "offline-device" };
  store.load({ imported: true, items: [pin, local, remote] });
  const key = (item: typeof pin | typeof local) => JSON.stringify([item.device, item.session_id]);
  expect(await store.move(key(remote), key(pin), "before")).toBe(true);
  expect(store.value.items).toEqual([remote, pin, local]);
  expect(save.mock.calls[0][0].items).toEqual([remote, pin, local]);
  save.mockRejectedValueOnce(new Error("disk full"));
  expect(await store.move(key(local), key(remote), "before")).toBe(false);
  expect(store.value.items).toEqual([remote, pin, local]);
  expect(store.error).toContain("置顶保存失败");
});
