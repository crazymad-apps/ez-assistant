import { afterEach, expect, it, vi } from "vitest";
import { observable, runInAction } from "mobx";
import { MIN_COMPATIBLE_VERSION, SOFTWARE_VERSION, type ApplicationSnapshot } from "@ez-assistant/protocol";
import { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import { NetworkDevicesStore } from "../../src/stores/NetworkDevicesStore";
import { NavigationStore } from "../../src/stores/NavigationStore";
import { RootStore } from "../../src/stores/RootStore";

const A = "11111111-1111-4111-8111-111111111111", B = "22222222-2222-4222-8222-222222222222";
const disposables: { dispose(): void }[] = [];
const application: ApplicationSnapshot = {
  runtime_lifecycle: "running", active_sessions_next_offset: null, archived_sessions_next_offset: null,
  configuration: { config_path: null, revision: "fixture", state: "ready", schema_version: 1, issues: [] },
  providers: [], model_settings: { default_model: null, vision_model: null }, workspaces: [], active_sessions: [], archived_sessions: [],
  controller_availability: { status: "unavailable" }, additional_controller_count: 0,
  capabilities: { conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true, tool_detail: true,
    queue_control: true, approval_queue: true, child_task_view: true, conversation_search: true },
};
function client() {
  const value = new RuntimeClient({ base_url: "http://127.0.0.1:7240", access_token: "fixture", login_context: "identity-1",
    instance_id: "same-source-instance", started_runtime: false, capabilities: { mode: "enterprise", runtime_version: SOFTWARE_VERSION,
      min_compatible_version: MIN_COMPATIBLE_VERSION, max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true, features: [] } });
  disposables.push(value);
  return value;
}
function fetcher() {
  const streams = new Map<string, ReadableStreamDefaultController<Uint8Array>>();
  let sequence = 80;
  const fetch = vi.fn(async (url: string, init: RequestInit = {}) => {
    const device = new Headers(init.headers).get("x-ez-target-device");
    if (url.endsWith("/capabilities")) return new Response("{}", { headers: { "x-ez-network-proxy": "2" } });
    if (url.endsWith("/events")) return new Response(new ReadableStream<Uint8Array>({ start(controller) {
      streams.set(device!, controller);
      init.signal?.addEventListener("abort", () => { try { controller.close(); } catch { /* Already closed. */ } }, { once: true });
    } }));
    const body = JSON.parse(String(init.body));
    return Response.json({ request_id: body.request_id, result: { scope: "runtime", payload: {
      type: body.command.payload.type, payload: { snapshot: { observed_sequence: sequence, value: {
        ...application, configuration: { ...application.configuration, revision: device ?? "local" },
      } } },
    } } });
  });
  vi.stubGlobal("fetch", fetch);
  return { fetch, streams, resetSequence() { sequence = 1; } };
}
afterEach(() => { disposables.splice(0).reverse().forEach((item) => item.dispose()); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it.each([false, true])("merges the connected Host with its directory entry (desktop=%s)", async (desktop) => {
  const local = client(), transport = fetcher();
  const network = new NetworkDevicesStore(() => local, new NavigationStore(), vi.fn(), () => desktop);
  disposables.push(network);
  const items = [A, B].map((id) => ({ id, name: id === A ? "工作 Mac" : "Windows 主机", platform: id === A ? "macos" : "windows", online: true, enabled: true, desktop_view: false, desktop_control: false }));
  transport.fetch.mockImplementation(async (url) => url.endsWith("/capabilities")
    ? new Response("{}", { headers: { "x-ez-network-proxy": "2" } })
    : Response.json({ type: "devices", payload: { self_device_id: A, items, next_offset: null } }));
  await network.refresh();
  expect(network.self_device).toBe(A);
  expect(network.online_devices.map((device) => device.id)).toEqual([B]);
  expect(network.label(null)).toBe(desktop ? "本机" : "工作 Mac");
  network.select(A);
  expect(network.browsing_device).toBeNull();
  expect(network.contexts.size).toBe(0);
  expect(transport.fetch.mock.calls.every(([, init]) => !new Headers(init?.headers).has("x-ez-target-device"))).toBe(true);

  // 未登记的直连 Host 仍可访问授权设备；不能凭名称、平台或列表位置去重。
  transport.fetch.mockImplementation(async (url) => url.endsWith("/capabilities")
    ? new Response("{}", { headers: { "x-ez-network-proxy": "2" } })
    : Response.json({ type: "devices", payload: { self_device_id: null, items, next_offset: null } }));
  await network.refresh();
  expect(network.online_devices).toHaveLength(2);
  expect(network.label(null)).toBe(desktop ? "本机" : "当前设备");
});

it("uses the direct Host draft when its registration ID is selected", () => {
  const root = new RootStore(); disposables.push(root);
  runInAction(() => { root.network.self_device = A; });
  root.openNewSessionDraft();
  const connect = vi.spyOn(root.network, "ensureConnected");
  root.switchDraftDevice(A);
  expect(root.navigation.target_device).toBeNull();
  expect(root.new_session_drafts).toBe(root.local_new_session_drafts);
  expect(connect).not.toHaveBeenCalled();
});

it("lists only online enabled choices without losing the selected offline device identity", () => {
  const navigation = new NavigationStore();
  const network = new NetworkDevicesStore(() => null, navigation, () => undefined);
  disposables.push(network);
  runInAction(() => {
    network.devices = [
      { id: A, name: "Windows 主机", platform: "windows", online: true, enabled: true, desktop_view: false, desktop_control: false },
      { id: B, name: "离线 Mac", platform: "macos", online: false, enabled: true, desktop_view: false, desktop_control: false },
      { id: "disabled", name: "停用主机", platform: "linux", online: true, enabled: false, desktop_view: false, desktop_control: false },
    ];
    network.browsing_device = B;
  });
  expect(network.online_devices.map((device) => device.id)).toEqual([A]);
  expect(network.label(network.browsing_device)).toBe("离线 Mac");
  runInAction(() => { network.devices = network.devices.map((device) => ({ ...device, online: false })); });
  expect(network.online_devices).toEqual([]);
  expect(network.browsing_device).toBe(B);
});

it("requires proxy v2 before any remote request and retains immutable request target", async () => {
  const local = client();
  const fetch = vi.fn().mockResolvedValue(new Response("{}"));
  vi.stubGlobal("fetch", fetch);
  await expect(local.forDevice(A)).rejects.toMatchObject({ code: "network_proxy_unavailable" });
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(new Headers(fetch.mock.calls[0]![1].headers).has("x-ez-target-device")).toBe(false);
  const transport = fetcher();
  const remote = await local.forDevice(A);
  disposables.push(remote);
  await remote.command({ type: "get_application_snapshot", payload: {} });
  const last = transport.fetch.mock.calls.at(-1)!;
  expect(last[0]).toBe("http://127.0.0.1:7240/commands");
  expect(new Headers(last[1]?.headers).get("x-ez-target-device")).toBe(A);
  expect(new Headers(last[1]?.headers).get("authorization")).toBe("Bearer fixture");
  const before = transport.fetch.mock.calls.length;
  await expect(remote.command({ type: "set_session_pinned", payload: { session_id: "blocked", is_pinned: true } })).rejects.toMatchObject({ code: "remote_operation_unavailable", target_device: A });
  await expect(remote.resource("/attachments/file", {}, async () => undefined)).rejects.toMatchObject({ code: "remote_operation_unavailable" });
  await expect(remote.logout()).rejects.toMatchObject({ code: "remote_operation_unavailable" });
  expect(transport.fetch).toHaveBeenCalledTimes(before);
});

it("preserves remote error target and unknown outcome without changing source authentication", async () => {
  const local = client(); fetcher();
  const remote = await local.forDevice(A); disposables.push(remote);
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({ error: { code: "access_revoked", message: "授权已撤销", target_device: A, outcome: "unknown" } }, { status: 403 })));
  await expect(remote.command({ type: "get_application_snapshot", payload: {} })).rejects.toMatchObject({ code: "access_revoked", target_device: A, outcome: "unknown" });
});

it("keeps sidebar and body targets independent and resets a reconnect to its own low sequence", async () => {
  const local = client(), transport = fetcher(), navigation = new NavigationStore();
  const network = new NetworkDevicesStore(() => local, navigation, vi.fn()); disposables.push(network);
  runInAction(() => { network.available = true; network.devices = [A, B].map((id) => ({ id, name: id, platform: "test", online: true, enabled: true, desktop_view: false, desktop_control: false })); });
  network.select(A); await network.ensureConnected(A);
  const a = network.context(A);
  navigation.selectSession("same-id", true, A);
  network.select(B); await network.ensureConnected(B);
  expect(navigation.target_device).toBe(A);
  expect(a.projection.application?.configuration.revision).toBe(A);
  expect(network.context(B).projection.application?.configuration.revision).toBe(B);
  // 撤掉选中会话以聚焦事件水位，不需要伪造会话正文 DTO。
  navigation.selectSession(null, false, A);
  transport.resetSequence();
  a.runtime.retryConnection();
  await vi.waitFor(() => expect(a.connection.state).toBe("connected"));
  const event = { sequence: 2, emitted_at_ms: 2, event: { type: "config_changed" } };
  transport.streams.get(A)!.enqueue(new TextEncoder().encode(`event: runtime_event\ndata: ${JSON.stringify(event)}\n\n`));
  const before = transport.fetch.mock.calls.length;
  await vi.waitFor(() => expect(transport.fetch.mock.calls.length).toBeGreaterThan(before));
  expect(a.projection.application?.configuration.revision).toBe(A);
  expect(network.context(B).connection.state).toBe("connected");
  expect(transport.fetch.mock.calls.some(([url]) => url.endsWith("/runtime/ensure-ready"))).toBe(false);
});

it("does not apply a late snapshot after its unused device has been released", async () => {
  const local = client(), transport = fetcher(), navigation = new NavigationStore();
  const network = new NetworkDevicesStore(() => local, navigation, vi.fn()); disposables.push(network);
  const a = network.context(A); await network.ensureConnected(A);
  let resolve!: (value: Response) => void;
  transport.fetch.mockImplementationOnce(() => new Promise<Response>((done) => { resolve = done; }));
  const pending = a.runtime.loadApplication();
  network.select(null);
  network.dispose();
  const request = JSON.parse(String(transport.fetch.mock.calls.at(-1)![1]?.body));
  resolve(Response.json({ request_id: request.request_id, result: { scope: "runtime", payload: { type: "get_application_snapshot", payload: { snapshot: { observed_sequence: 999, value: application } } } } }));
  await pending;
  expect(a.projection.application?.configuration.revision).toBe(A);
  expect(network.contexts.size).toBe(0);
});

it("keeps navigation for identical session IDs separate across devices", () => {
  const navigation = new NavigationStore();
  navigation.selectSession("same", true, null);
  navigation.updateCurrentScrollOffset(12);
  navigation.selectSession("same", true, A);
  navigation.updateCurrentScrollOffset(90);
  expect(navigation.conversation_history).toHaveLength(2);
  expect(navigation.goBack()).toMatchObject({ target_device: null, scroll_offset: 12 });
  expect(navigation.goForward()).toMatchObject({ target_device: A, scroll_offset: 90 });
  navigation.clearConversationLocations("same", null);
  expect(navigation.conversation_history).toHaveLength(1);
  expect(navigation.conversation_history[0]?.target_device).toBe(A);
});

it("local resources and settings remain owned by the local connection when viewing remote", () => {
  const root = new RootStore(); disposables.push(root);
  const files = root.files, resources = root.resource_workspace;
  const context = root.network.context(A);
  runInAction(() => root.navigation.selectSession("remote", true, A));
  expect(root.files).toBe(context.files);
  expect(root.files.desktop).toBe(false);
  expect(root.local_files).toBe(files);
  expect(root.resource_workspace).toBe(context.resources);
  expect(root.local_resource_workspace).toBe(resources);
  expect(root.connection).toBe(context.connection);
  expect(root.local_connection).not.toBe(context.connection);
});


it("keeps device drafts separate and transfers local file selections only to an empty destination", async () => {
  const root = new RootStore(); disposables.push(root);
  const local = client(); fetcher();
  vi.spyOn(root, "local_runtime_client", "get").mockReturnValue(local);
  vi.spyOn(root.network, "ensureConnected").mockResolvedValue();
  runInAction(() => { root.network.available = true; root.network.devices = [A, B].map((id) => ({ id, name: id, platform: "test", online: true, enabled: true, desktop_view: false, desktop_control: false })); });
  root.openNewSessionDraft();
  root.local_new_session_drafts.updateText("unbound", "本机未发送内容");
  root.local_new_session_drafts.updateModel("unbound", { provider_instance_id: "local-model", model_id: "only-here" });
  const selection = await root.local_files.stageClipboardImage(new File(["bytes"], "image.png", { type: "image/png" }));
  root.local_new_session_drafts.updateAttachments("unbound", [{ ...selection, media_type: "image/png", origin: "clipboard", state: "selected", attachment_id: null, operation_id: null, error: null }]);
  root.switchDraftDevice(A);
  expect(root.navigation.target_device).toBe(A);
  expect(root.new_session_drafts.get("unbound")).toMatchObject({ text: "本机未发送内容", model_selection: null });
  expect(root.local_new_session_drafts.get("unbound")).toBeNull();
  await expect(root.files.previewAttachmentSelection(selection.selection_id)).resolves.toMatchObject({ kind: "image" });
  await expect(root.local_files.previewAttachmentSelection(selection.selection_id)).rejects.toThrow("失效");
  root.local_new_session_drafts.open("unbound");
  root.local_new_session_drafts.updateText("unbound", "另一本机草稿");
  root.switchDraftDevice(null);
  expect(root.new_session_drafts.get("unbound")?.text).toBe("另一本机草稿");
  expect(root.network.context(A).drafts.get("unbound")?.text).toBe("本机未发送内容");
  expect(root.network.context(A).drafts.get("unbound")?.attachments).toHaveLength(1);
  root.network.select(B);
  expect(root.new_session_drafts).toBe(root.local_new_session_drafts);
});

it("routes writes and files to a frozen device while management stays local", async () => {
  const local = client(), transport = fetcher();
  const remote = await local.forDevice(A); disposables.push(remote);
  await remote.command({ type: "rename_session", payload: { session_id: "same", title: "remote" } });
  expect(new Headers(transport.fetch.mock.calls.at(-1)![1]?.headers).get("x-ez-target-device")).toBe(A);
  transport.fetch.mockResolvedValueOnce(new Response("file-content"));
  await expect(remote.resource("/host-files/download", { method: "POST", headers: { "x-ez-target-device": B }, body: "{}" }, (response) => response.text())).resolves.toBe("file-content");
  expect(new Headers(transport.fetch.mock.calls.at(-1)![1]?.headers).get("x-ez-target-device")).toBe(A);
  await expect(remote.session()).rejects.toMatchObject({ code: "remote_operation_unavailable" });
});


it("retains a device while its project editor is open after sidebar and body move away", async () => {
  const local = client(); fetcher();
  const editing = observable.box<string | null>(null);
  const network = new NetworkDevicesStore(() => local, new NavigationStore(), vi.fn(), undefined, undefined, () => editing.get());
  disposables.push(network);
  runInAction(() => { network.available = true; network.devices = [{ id: A, name: "remote", platform: "test", online: true, enabled: true, desktop_view: false, desktop_control: false }]; });
  network.select(A); await network.ensureConnected(A);
  const original = network.context(A);
  runInAction(() => editing.set(A));
  network.select(null);
  expect(network.contexts.get(A)).toBe(original);
  expect(original.runtime.client).not.toBeNull();
  runInAction(() => editing.set(null));
  expect(network.contexts.has(A)).toBe(false);
});

it("right resources follow body navigation while sidebar browsing and hidden terminals retain their targets", () => {
  const local = client(); fetcher();
  const root = new RootStore(); disposables.push(root);
  vi.spyOn(root.network, "ensureConnected").mockResolvedValue(undefined);
  const remote = root.network.context(A);
  root.navigation.selectSession("remote-a", true, A);
  remote.resources.openWorkspace("session:remote-a");
  remote.resources.openTerminal({ type: "workspace", workspace_id: "project-a" }, undefined, true);
  const terminal = [...remote.resources.terminals.values()][0]!;
  root.network.select(null);
  expect(root.reading_device).toBe(A);
  expect(root.reading_scope).toBe("session:remote-a");
  root.navigation.selectSession("local-session", true, null);
  expect(root.reading_resources).toBe(root.local_resource_workspace);
  expect(root.reading_scope).toBe("session:local-session");
  expect(root.local_resource_workspace.tabs).toHaveLength(1);
  expect([...remote.resources.terminals.values()]).toContain(terminal);
  root.navigation.selectSession("remote-b", true, A);
  expect(root.reading_scope).toBe("session:remote-b");
  expect(remote.resources.tabs).toHaveLength(1);
  root.navigation.selectSession("remote-a", true, A);
  expect(remote.resources.tabs.some((tab) => tab.type === "terminal")).toBe(true);
  expect(local.target_device).toBeNull();
});

it("uses the fixed device for Shell discovery and keeps management endpoints local", async () => {
  const local = client(); fetcher();
  const remote = await local.forDevice(A); disposables.push(remote);
  const fetch = vi.fn().mockResolvedValue(Response.json([{ shell: null, name: "zsh" }]));
  vi.stubGlobal("fetch", fetch);
  await remote.listUserTerminalShells(new AbortController().signal);
  expect(fetch.mock.calls[0]![0]).toBe("http://127.0.0.1:7240/user-terminals/shells");
  expect(new Headers(fetch.mock.calls[0]![1].headers).get("x-ez-target-device")).toBe(A);
  await expect(remote.resource("/user-terminals/admin", {}, async () => undefined)).rejects.toMatchObject({ code: "remote_operation_unavailable" });
});
