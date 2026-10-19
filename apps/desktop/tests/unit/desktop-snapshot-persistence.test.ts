import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { RootStore } from "../../src/stores/RootStore";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { SessionManagementController } from "../../src/stores/SessionManagementController";
import type { DesktopPreferences } from "../../src/native-bridge/desktopPreferences";
const bridge = vi.hoisted(() => ({ load: vi.fn(), save: vi.fn() }));
vi.mock("../../src/native-bridge/desktopPreferences", async (original) => ({...await original<typeof import("../../src/native-bridge/desktopPreferences")>(), loadDesktopPreferences: bridge.load, saveDesktopPreferences: bridge.save }));
const stores: RootStore[] = [];
const defaults: DesktopPreferences = { left_sidebar_open: true, right_sidebar_open: true, left_sidebar_width: 286, right_sidebar_width: 380, expanded_workspace_ids: [], close_behavior: "hide_to_tray", default_approval_mode: "ask", last_model_selection: null };
beforeEach(() => { vi.spyOn(RuntimeLifecycleCoordinator.prototype, "connect").mockResolvedValue(); bridge.load.mockResolvedValue(defaults); bridge.save.mockResolvedValue(undefined); });
afterEach(async () => { for (const store of stores.splice(0)) { await store.flushPreferences(); store.dispose(); } vi.restoreAllMocks(); vi.clearAllMocks(); });
function create(options: ConstructorParameters<typeof RootStore>[0] = undefined) {
  const store = new RootStore(options);
  vi.mocked(RuntimeLifecycleCoordinator.prototype.connect).mockImplementationOnce(async () => {
    store.connection.markConnected("fixture", { mode: "personal", min_compatible_version: "0.25.2", runtime_version: "0.25.2", max_command_bytes: 1000, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["startup_diagnostics"] });
  });
  stores.push(store); return store;
}
it("restores per-target approval and model defaults only into newly created drafts", async () => {
  const remembered = { provider_instance_id: "provider-1", model_id: "model-a" };
  bridge.load.mockResolvedValueOnce({
    ...defaults,
    default_approval_mode: "auto",
    last_model_selection: remembered,
  });
  const store = create({ target_kind: "remote", target_address: "https://host.example" });
  await store.connect();
  store.openNewSessionDraft(null);
  expect(store.new_session_drafts.get("unbound")).toMatchObject({
    approval_mode: "auto",
    model_selection: remembered,
  });

  store.setNewSessionDraftApprovalMode("unbound", "ask");
  store.setNewSessionDraftModel("unbound", null);
  store.new_session_drafts.open("workspace:workspace-2");
  expect(store.new_session_drafts.get("workspace:workspace-2")).toMatchObject({
    approval_mode: "ask",
    model_selection: null,
  });
  await store.flushPreferences();
  expect(bridge.load).toHaveBeenCalledWith("https://host.example");
  expect(bridge.save).toHaveBeenLastCalledWith(expect.objectContaining({
    default_approval_mode: "ask",
    last_model_selection: null,
  }), "https://host.example");
});

it("updates defaults only after a materialized Session mutation succeeds", async () => {
  const store = create();
  await store.connect();
  vi.spyOn(SessionManagementController.prototype, "setSessionApprovalMode")
    .mockResolvedValueOnce(true)
    .mockResolvedValueOnce(false);
  vi.spyOn(SessionManagementController.prototype, "setSessionModel")
    .mockResolvedValueOnce(true)
    .mockResolvedValueOnce(false);
  const remembered = { provider_instance_id: "provider-1", model_id: "model-a" };

  expect(await store.setSessionApprovalMode("session-1", "auto")).toBe(true);
  expect(await store.setSessionModel("session-1", remembered)).toBe(true);
  expect(await store.setSessionApprovalMode("session-1", "ask")).toBe(false);
  expect(await store.setSessionModel("session-1", null)).toBe(false);
  expect(store.new_session_drafts.default_approval_mode).toBe("auto");
  expect(store.new_session_drafts.default_model_selection).toEqual(remembered);
});

it("keeps the in-memory default and reports a visible error when preference saving fails", async () => {
  const store = create();
  await store.connect();
  store.openNewSessionDraft(null);
  store.setNewSessionDraftApprovalMode("unbound", "auto");
  bridge.save.mockRejectedValueOnce(new Error("disk full"));

  await expect(store.flushPreferences()).rejects.toThrow("disk full");
  expect(store.new_session_drafts.default_approval_mode).toBe("auto");
  expect(store.new_session_drafts.get("unbound")?.approval_mode).toBe("auto");
  expect(store.interaction_error).toContain("桌面状态保存失败");
});

it("clears a remembered model when its provider disappears without rewriting meaningful drafts", async () => {
  const remembered = { provider_instance_id: "provider-removed", model_id: "model-a" };
  bridge.load.mockResolvedValueOnce({ ...defaults, last_model_selection: remembered });
  const store = create();
  await store.connect();
  store.openNewSessionDraft(null);
  store.new_session_drafts.open("workspace:workspace-2");
  store.new_session_drafts.updateText("workspace:workspace-2", "保留内容");

  store.projection.applyApplicationSnapshot({
    observed_sequence: 1,
    value: { providers: [{ provider_instance_id: "provider-removed" }] },
  } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);
  expect(store.new_session_drafts.default_model_selection).toEqual(remembered);

  store.projection.applyApplicationSnapshot({
    observed_sequence: 2,
    value: { providers: [] },
  } as unknown as Parameters<RootStore["projection"]["applyApplicationSnapshot"]>[0]);

  expect(store.new_session_drafts.default_model_selection).toBeNull();
  expect(store.new_session_drafts.get("unbound")?.model_selection).toBeNull();
  expect(store.new_session_drafts.get("workspace:workspace-2")?.model_selection).toEqual(remembered);
  expect(store.interaction_error).toContain("服务商已删除");
});
it("starts in a new draft while retaining saved sessions' resource tabs", async () => {
  let load!: (value: DesktopPreferences) => void;
  bridge.load.mockImplementationOnce(() => new Promise((resolve) => { load = resolve; }));
  const store = create();
  const initialized = store.initializePreferences();
  const connected = store.connect(); const repeated = store.connect();
  await store.flushPreferences();
  expect(bridge.save).not.toHaveBeenCalled();
  expect(RuntimeLifecycleCoordinator.prototype.connect).not.toHaveBeenCalled();
  load({ ...defaults, resource_workspace: { current_scope_key: "session:saved", groups: [{ scope_key: "session:saved", active_index: 1, focused_index: 1, tabs: [{ page: { type: "context" } }, { page: { type: "workspace" } }] }] } });
  await Promise.all([initialized, connected, repeated]);
  expect(bridge.load).toHaveBeenCalledOnce();
  expect(RuntimeLifecycleCoordinator.prototype.connect).toHaveBeenCalledOnce();
  expect(store.navigation.selected_session_id).toBeNull();
  expect(store.navigation.selected_draft_key).toBe("unbound");
  expect(store.resource_workspace.active_tab.type).toBe("context");
  expect(store.resource_workspace.groups.get("session:saved")?.tabs.map((tab) => tab.type)).toEqual(["context", "workspace"]);
  await store.flushPreferences();
  expect(bridge.save.mock.calls[0]?.[0].desktop_workspace.devices[0].resources.current_scope_key).toBe("draft:unbound");
});
it("starts with automatic approval when preferences cannot be loaded and retains the error", async () => {
  bridge.load.mockRejectedValueOnce(new Error("invalid preferences"));
  const store = create();
  await store.connect();
  expect(store.navigation.selected_draft_key).toBe("unbound");
  expect(store.new_session_drafts.get("unbound")?.approval_mode).toBe("auto");
  expect(store.interaction_error).toContain("桌面状态读取失败");
});

it("serializes disk writes and freezes the final snapshot before terminal shutdown removes its tab", async () => {
  const store = create(); await store.connect();
  store.navigation.selectSession("a");
  store.resource_workspace.openTerminal({ type: "workspace", workspace_id: "workspace" }, undefined, true);
  let finish!: () => void;
  bridge.save.mockImplementationOnce(() => new Promise<void>((resolve) => { finish = resolve; }));
  const first = store.flushPreferences(); await Promise.resolve();
  store.resource_workspace.openWorkspace("session:a");
  const second = store.flushPreferences(); await Promise.resolve();
  expect(bridge.save).toHaveBeenCalledTimes(1);
  await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
  finish(); await Promise.all([first, second]);
  expect(bridge.save).toHaveBeenCalledTimes(2);
  const persisted = bridge.save.mock.calls[1]![0];
  await store.resource_workspace.shutdownTerminals();
  await store.flushPreferences();
  expect(bridge.save).toHaveBeenCalledTimes(2);
  expect(persisted.desktop_workspace.devices[0].resources.groups.find((group: { scope_key: string }) => group.scope_key === "session:a").tabs.map((tab: { page: { type: string } }) => tab.page.type)).toEqual(["context", "terminal", "workspace"]);
});

vi.mock("../../src/runtime-client/ClientResources", async (original) => {
  const actual = await original<typeof import("../../src/runtime-client/ClientResources")>();
  return {...actual, ClientResources: class extends actual.ClientResources { override readonly desktop = true; }};
});


it("preserves invalid identity snapshots instead of replacing them with an empty working state", async () => {
  bridge.load.mockResolvedValueOnce({ ...defaults, desktop_workspace: { version: 1, owner: "another-account", pins: { imported: true, items: [] }, selection: null, devices: [] } });
  const store = create({ target_kind: "remote", target_address: "https://host.example" });
  await store.connect();
  await store.flushPreferences();
  expect(bridge.save).not.toHaveBeenCalled();
  expect(store.interaction_error).toContain("未覆盖");
});

it("retains device expansion and draft defaults when an unused connection is released", async () => {
  const device = "11111111-1111-4111-8111-111111111111";
  const store = create(); await store.connect();
  const context = store.network.context(device);
  context.sidebar.toggleWorkspace("project-a");
  context.drafts.applyDefaults("ask", { provider_instance_id: "remote-provider", model_id: "remote-model" });
  store.navigation.selectSession(null, false, device);
  store.navigation.selectSession(null, false, null);
  expect(store.network.contexts.has(device)).toBe(false);
  const restored = store.network.context(device);
  expect(restored.sidebar.expanded_workspaces.has("project-a")).toBe(true);
  expect(restored.drafts.default_model_selection).toEqual({ provider_instance_id: "remote-provider", model_id: "remote-model" });
  expect(restored.drafts.default_approval_mode).toBe("ask");
  expect(store.local_new_session_drafts.default_model_selection).toBeNull();
});


it("ignores old fixed/project resource selections while retaining session tabs and following navigation", async () => {
  bridge.load.mockResolvedValueOnce({ ...defaults, desktop_workspace: {
    version: 1, owner: "local", pins: { imported: true, items: [] },
    selection: { device: null, scope: "session:saved" }, devices: [{ device: null, expanded: ["project-a"], resources: {
      current_scope_key: "project:orphan", groups: [
        { scope_key: "project:orphan", active_index: 1, focused_index: 1, tabs: [{ page: { type: "context" } }, { page: { type: "workspace" } }] },
        { scope_key: "session:saved", active_index: 1, focused_index: 1, tabs: [{ page: { type: "context" } }, { page: { type: "workspace" } }] },
      ],
    } }],
  } });
  const store = create(); await store.connect();
  expect(store.reading_scope).toBe("draft:unbound");
  expect(store.reading_resources.groups.has("project:orphan")).toBe(false);
  expect(store.reading_resources.groups.get("session:saved")?.tabs).toHaveLength(2);
  expect(store.navigation.expanded_workspaces.has("project-a")).toBe(true);
  store.navigation.selectSession("saved");
  expect(store.reading_resources.active_tab.type).toBe("workspace");
  store.navigation.navigateTo({ session_id: "notification", child_task_id: null, anchor_message_id: null, scroll_offset: null });
  expect(store.reading_scope).toBe("session:notification");
  store.navigation.goBack();
  expect(store.reading_scope).toBe("session:saved");
  store.navigation.goForward();
  expect(store.reading_scope).toBe("session:notification");
  store.openNewSessionDraft(null);
  expect(store.reading_scope).toBe("draft:unbound");
  await store.flushPreferences();
  expect(bridge.save.mock.calls.at(-1)?.[0].desktop_workspace).not.toHaveProperty("selection");
});
