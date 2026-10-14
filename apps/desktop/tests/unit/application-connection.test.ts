import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { observable, runInAction } from "mobx";
import type { LocalHostStatus } from "../../src/native-bridge/runtimeConnection";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";

const native = vi.hoisted(() => ({
  desktop: true,
  manage: vi.fn(), restart: vi.fn(),
  local: vi.fn<() => Promise<RuntimeBootstrap>>(), begin: vi.fn<() => Promise<string>>(),
  upgrade: vi.fn<() => Promise<RuntimeBootstrap>>(), prepare: vi.fn(async () => undefined),
  connect: vi.fn<(binding: string, origin: string | null, password: string, remember: boolean) => Promise<{ bootstrap: RuntimeBootstrap; warning: string | null }>>(),
  activate: vi.fn(), clear: vi.fn(), restore: vi.fn(async (): Promise<RuntimeBootstrap | null> => null),
}));
const update = vi.hoisted(() => ({ read: vi.fn(), listen: vi.fn(), start: vi.fn(), cancel: vi.fn() }));
vi.mock("../../src/native-bridge/desktopUpdate", () => ({ readDesktopUpdate: update.read, listenDesktopUpdate: update.listen, startDesktopUpdate: update.start, cancelDesktopUpdate: update.cancel }));
const web = vi.hoisted(() => ({ login: vi.fn(), bootstrap: vi.fn(), logout: vi.fn(), token: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => native.desktop }));
vi.mock("../../src/runtime-client/webLogin", () => ({
  discoverWebHost: vi.fn(async () => ({ mode: "personal" })), loginWeb: web.login, bootstrapWebRuntime: web.bootstrap, logoutWeb: web.logout, takeWebLoginToken: web.token,
  WebLoginError: class extends Error {},
}));
vi.mock("../../src/native-bridge/desktopLifecycle", () => ({ restartNativeRuntime: native.restart }));
vi.mock("../../src/native-bridge/runtimeBootstrap", () => ({ bootstrapRuntime: native.local, upgradeRuntime: native.upgrade }));
vi.mock("../../src/native-bridge/runtimeConnection", () => ({ beginRuntimeConnection: native.begin, connectRuntimeTarget: native.connect, activateRuntimeBinding: native.activate, clearRuntimeBinding: native.clear, openRuntimeWeb: vi.fn(), restoreRuntimeConnection: native.restore, probeRuntimeTarget: vi.fn(), prepareDesktopUpdate: native.prepare, manageLocalHost: native.manage }));
vi.mock("../../src/stores/RootStore", () => ({ RootStore: class {
  constructor(readonly options: { flush_application_preferences?: () => Promise<void> } = {}) {}
  connection = observable({ markDisconnected: vi.fn(), state: "disconnected", last_error_code: null as string | null, error_message: null as string | null, address: "" });
  settings = { open: vi.fn(), close: vi.fn(), showNotice: vi.fn() };
  dispose = vi.fn();
  connect = vi.fn(async (bootstrap: RuntimeBootstrap) => { runInAction(() => { this.connection.state = "connected"; this.connection.address = bootstrap.base_url; }); });
} }));
import { RootStore } from "../../src/stores/RootStore";
import { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";

const bootstrap = (origin: string): RuntimeBootstrap => ({ base_url: origin, instance_id: origin, access_token: "fixture", capabilities: { mode: "personal", min_compatible_version: "0.25.2", runtime_version: "0.25.2", max_command_bytes: 1048576, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["web_login"] }, started_runtime: false, session: { token: null, expires_at_ms: 0, instance_id: origin, mode: "personal", kind: "user", identity: null, login_context: origin } });
const deferred = <T>() => { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; };
beforeEach(() => {
  update.read.mockResolvedValue({ revision: 1, phase: "idle", available: false, required: false, notes: "", received: 0, total: 0, error: null, resume_remote: null, stops_local_host: false, binding_id: null, host_mode: null });
  update.listen.mockResolvedValue(() => undefined);
  vi.stubGlobal("BroadcastChannel", undefined); vi.clearAllMocks(); native.desktop = true; native.local.mockResolvedValue(bootstrap("http://local")); native.begin.mockResolvedValue("binding"); native.connect.mockImplementation(async (_binding, origin) => ({ bootstrap: bootstrap(origin ?? "http://local"), warning: null })); web.login.mockResolvedValue(undefined); web.logout.mockResolvedValue(undefined); web.token.mockReturnValue(null); web.bootstrap.mockResolvedValue(bootstrap("http://web")); });

afterEach(() => { vi.unstubAllGlobals(); });

describe("Desktop connection entry and switching", () => {
  it("detects an upgrade without stopping the Host, then deduplicates explicit upgrade and concurrent startup", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    native.local.mockRejectedValueOnce({ code: "runtime_upgrade_required", message: "需要完成本机更新" });
    await expect(entry.startLocal()).rejects.toMatchObject({ code: "runtime_upgrade_required" });
    expect(entry.local_state).toBe("upgrade_required");
    expect(native.upgrade).not.toHaveBeenCalled();
    const pending = deferred<RuntimeBootstrap>();
    native.upgrade.mockReturnValueOnce(pending.promise);
    const upgrade = entry.upgradeLocal();
    expect(entry.upgradeLocal()).toBe(upgrade);
    expect(entry.startLocal(true)).toBe(upgrade);
    expect(entry.local_state).toBe("starting");
    pending.resolve(bootstrap("http://local"));
    await upgrade;
    expect(native.upgrade).toHaveBeenCalledOnce();
    expect(entry.local_state).toBe("ready");
    expect(entry.local_error).toBeNull();
    entry.dispose();
  });

  it("keeps upgrade failure visible and retries by detecting the current Host again", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    native.upgrade.mockRejectedValueOnce({ message: "旧 Runtime 未停止" });
    await expect(entry.upgradeLocal()).rejects.toMatchObject({ message: "旧 Runtime 未停止" });
    expect(entry.local_state).toBe("failed");
    expect(entry.local_error).toBe("旧 Runtime 未停止");
    await entry.startLocal();
    expect(entry.local_state).toBe("ready");
    entry.dispose();
  });
  it("returns an expired restored native token to login instead of initialization retry", async () => {
    const { session: _session, ...saved } = bootstrap("http://local");
    native.restore.mockResolvedValueOnce(saved);
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: { code: "authentication_required", message: "expired" } }), { status: 401, headers: { "Content-Type": "application/json" } })));
    const entry = new ApplicationConnectionStore(new RootStore());
    await entry.initialize();
    expect(entry.phase).toBe("entry");
    expect(entry.session).toBeNull();
    expect(entry.can_retry).toBe(false);
    expect(entry.error).toBe("登录已失效，请重新登录。");
    entry.dispose();
  });
  it("restores a personal native binding without disposing the initial Store first", async () => {
    const root = new RootStore(), entry = new ApplicationConnectionStore(root);
    native.restore.mockResolvedValueOnce(bootstrap("http://local"));
    await entry.initialize();
    expect(entry.current).toBe(root);
    expect(root.dispose).not.toHaveBeenCalled();
    expect(root.connect).toHaveBeenCalledOnce();
    expect(entry.phase).toBe("workspace");
    entry.dispose();
  });
  it("clears the previous startup error when local startup is retried successfully", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    native.local.mockRejectedValueOnce({ message: "Runtime 启动超时" });
    await entry.connectDesktop(null, "", false);
    expect(entry.error).toBe("Runtime 启动超时");
    expect(entry.local_state).toBe("failed");
    await entry.startLocal();
    expect(entry.local_state).toBe("ready");
    expect(entry.local_error).toBeNull();
    expect(entry.error).toBeNull();
    await entry.connectDesktop(null, "", false);
    expect(entry.phase).toBe("workspace");
    entry.dispose();
  });
  it("silently starts local Runtime but waits for explicit selection", async () => {
    const root = new RootStore(), entry = new ApplicationConnectionStore(root);
    await entry.initialize();
    expect(native.local).toHaveBeenCalledOnce();
    expect(entry.phase).toBe("entry");
    expect(root.connect).not.toHaveBeenCalled();
    await entry.connectDesktop(null, "", false);
    expect(entry.phase).toBe("workspace");
    expect(root.dispose).toHaveBeenCalled();
    expect(entry.current?.settings.close).toHaveBeenCalledOnce();
    entry.dispose();
  });
  it("returns a successful settings switch to the new Runtime overview", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    await entry.initialize();
    await entry.connectDesktop(null, "", false);
    await entry.connectDesktop("http://remote", "password", false);
    expect(entry.phase).toBe("workspace");
    expect(entry.current?.connection.address).toBe("http://remote");
    expect(entry.current?.settings.open).toHaveBeenLastCalledWith("runtime");
    expect(entry.current?.settings.close).not.toHaveBeenCalled();
    entry.dispose();
  });
  it("clears the old projection and returns to entry after a failed target login", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    await entry.connectDesktop(null, "", false); const previous = entry.current;
    native.connect.mockRejectedValueOnce({ code: "authentication_required", message: "密码错误" });
    await entry.connectDesktop("http://remote", "wrong", false);
    expect(previous?.dispose).toHaveBeenCalled();
    expect(entry.current).not.toBe(previous);
    expect(entry.phase).toBe("entry");
    expect(entry.error).toBe("密码错误");
    expect(native.connect).toHaveBeenLastCalledWith("binding", "http://remote", "wrong", false, "");
    entry.dispose();
  });
  it("ignores a late remote login after another target is selected", async () => {
    const late = deferred<{ bootstrap: RuntimeBootstrap; warning: string | null }>();
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    native.connect.mockReturnValueOnce(late.promise);
    const pending = entry.connectDesktop("http://remote", "password", true);
    await vi.waitFor(() => expect(native.connect).toHaveBeenCalledOnce());
    entry.chooseTarget("local"); await entry.connectDesktop(null, "", false);
    const current = entry.current;
    late.resolve({ bootstrap: bootstrap("http://remote"), warning: null }); await pending;
    expect(entry.current).toBe(current);
    expect(native.activate).toHaveBeenCalledOnce();
    expect(entry.current?.connection.address).toBe("http://local");
    entry.dispose();
  });
  it("expired remote login clears business projection and asks for login", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    await entry.connectDesktop("http://remote", "password", false); const previous = entry.current!;
    runInAction(() => { previous.connection.last_error_code = "authentication_required"; });
    expect(previous.dispose).toHaveBeenCalled();
    expect(entry.phase).toBe("entry");
    expect(entry.current).not.toBe(previous);
    entry.dispose();
  });
});

describe("Web login lifecycle", () => {
  beforeEach(() => { native.desktop = false; });

  it("focus revalidation does not retry failed initialization for the same login", async () => {
    const root = new RootStore(), entry = new ApplicationConnectionStore(root);
    vi.mocked(root.connect).mockImplementationOnce(async () => {
      runInAction(() => { root.connection.last_error_code = "runtime_startup_failed"; root.connection.error_message = "初始化失败"; });
    });
    await entry.login("fixture");
    expect(entry.can_retry).toBe(true);
    await entry.recheckSession();
    expect(entry.current).toBe(root);
    expect(root.connect).toHaveBeenCalledOnce();
    expect(entry.error).toBe("初始化失败");
    entry.dispose();
  });

  it("does not contact logout before explicit confirmation and cancel is side-effect free", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    await entry.login("fixture");
    const store = entry.current;
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    await entry.signOut();
    expect(entry.logout_confirmation).toBe(true);
    expect(fetcher).not.toHaveBeenCalled();
    entry.cancelLogout();
    expect(entry.current).toBe(store);
    expect(store?.dispose).not.toHaveBeenCalled();
    expect(entry.phase).toBe("workspace");
    entry.dispose();
  });

  it("does not reopen the workspace when a password response arrives after sign-out", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    const delayed = deferred<void>();
    web.login.mockReturnValueOnce(delayed.promise);
    const login = entry.login("fixture");
    await entry.signOut(false);
    delayed.resolve(); await login;
    expect(entry.phase).toBe("login");
    expect(entry.current).toBeNull();
    expect(web.bootstrap).not.toHaveBeenCalled();
    entry.dispose();
  });

  it("ignores an old bootstrap after a newer login has entered", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    const delayed = deferred<RuntimeBootstrap>();
    web.bootstrap.mockReturnValueOnce(delayed.promise);
    const old = entry.login("old");
    await vi.waitFor(() => expect(web.bootstrap).toHaveBeenCalledOnce());
    await entry.signOut(false);
    await entry.login("new");
    const current = entry.current;
    delayed.resolve(bootstrap("http://stale")); await old;
    expect(entry.current).toBe(current);
    expect(entry.current?.connection.address).toBe("http://web");
    expect(entry.pending).toBe(false);
    entry.dispose();
  });

  it("ignores a token bootstrap after the document is disposed", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    const delayed = deferred<RuntimeBootstrap>();
    web.token.mockReturnValueOnce("fixture-token");
    web.bootstrap.mockReturnValueOnce(delayed.promise);
    const accepting = entry.acceptTokenLink();
    await vi.waitFor(() => expect(web.bootstrap).toHaveBeenCalledOnce());
    entry.dispose();
    delayed.resolve(bootstrap("http://stale")); await accepting;
    expect(entry.current).toBeNull();
  });
});

describe("Update admission", () => {
  it("retains native admission while reconnecting the same target during a network failure", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    entry.updates.receive({ ...entry.updates.snapshot, revision: 2, required: true, binding_id: "binding" });
    const begins = native.begin.mock.calls.length;
    native.prepare.mockRejectedValueOnce(new Error("offline"));
    await entry.retryUpdateConnection();
    expect(entry.update_required).toBe(true); expect(entry.phase).toBe("entry");
    expect(native.begin).toHaveBeenCalledTimes(begins);
    expect(native.prepare).toHaveBeenLastCalledWith("binding", null);
    entry.updates.receive({ ...entry.updates.snapshot, revision: 3, required: false });
    expect(entry.update_required).toBe(false); entry.dispose();
  });

  it("leaves the workspace without logging out or cancelling the native update", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    await entry.initialize(); await entry.connectDesktop(null, "", false);
    const previous = entry.current!;
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    entry.updates.receive({ ...entry.updates.snapshot, revision: 2, required: true, available: true, binding_id: "binding" });
    expect(previous.dispose).toHaveBeenCalled();
    expect(entry.phase).toBe("entry"); expect(entry.update_required).toBe(true);
    entry.updates.show(); entry.updates.hide();
    const attempts = native.connect.mock.calls.length;
    await entry.retryInitialization(); await entry.connectDesktop(null, "new-password", false);
    expect(native.connect).toHaveBeenCalledTimes(attempts);
    expect(fetcher).not.toHaveBeenCalled(); expect(update.cancel).not.toHaveBeenCalled();
    entry.dispose();
  });

  it.each(["authentication_required", "center_unavailable", "center_identity_mismatch", "runtime_unavailable", "center_protocol_incompatible"])("keeps the exact classification of %s", async (code) => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    native.connect.mockRejectedValueOnce({ code, message: "fixture" });
    await entry.connectDesktop("http://remote", "fixture", false);
    expect(entry.update_required).toBe(code === "center_protocol_incompatible");
    expect(entry.phase).toBe("entry");
    if (code !== "center_protocol_incompatible") expect(entry.error).toBe("fixture");
    entry.dispose();
  });

  it("rejects a late successful login after the active target requires an update", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    const late = deferred<{ bootstrap: RuntimeBootstrap; warning: string | null }>();
    native.connect.mockReturnValueOnce(late.promise);
    const connecting = entry.connectDesktop("http://remote", "fixture", false);
    await vi.waitFor(() => expect(native.connect).toHaveBeenCalledOnce());
    entry.updates.receive({ ...entry.updates.snapshot, revision: 2, required: true, binding_id: "binding", resume_remote: "http://remote" });
    late.resolve({ bootstrap: bootstrap("http://remote"), warning: null }); await connecting;
    expect(entry.phase).toBe("entry"); expect(entry.update_required).toBe(true);
    expect(native.activate).not.toHaveBeenCalled();
    entry.dispose();
  });

  it.each([false, true])("restores a pre-login native download across a WebView reload (required=%s)", async (required) => {
    update.read.mockResolvedValue({ revision: 7, phase: "downloading", available: true, required, notes: "", received: 40, total: 100, error: null, resume_remote: "http://remote", stops_local_host: false, binding_id: "existing", host_mode: "enterprise" });
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    expect(entry.target).toBe("remote"); expect(entry.address).toBe("http://remote");
    expect(entry.mode).toBe("enterprise"); expect(entry.updates.snapshot.received).toBe(40);
    expect(native.begin).not.toHaveBeenCalled(); expect(native.local).not.toHaveBeenCalled(); expect(update.cancel).not.toHaveBeenCalled();
    await entry.connectDesktop("http://remote", "fixture", false);
    expect(native.connect).not.toHaveBeenCalled();
    entry.dispose();
  });

  it("blocks a restored authenticated connection before rebuilding its workspace", async () => {
    update.read.mockResolvedValue({ revision: 8, phase: "idle", available: false, required: true, notes: "", received: 0, total: 0, error: null, resume_remote: null, stops_local_host: true, binding_id: "existing", host_mode: "enterprise" });
    native.restore.mockResolvedValueOnce(bootstrap("http://local"));
    const initial = new RootStore(), entry = new ApplicationConnectionStore(initial); await entry.initialize();
    expect(initial.connect).not.toHaveBeenCalled(); expect(entry.phase).toBe("entry");
    expect(entry.update_required).toBe(true); expect(native.begin).not.toHaveBeenCalled();
    entry.dispose(); native.restore.mockReset(); native.restore.mockResolvedValue(null);
  });

  it("keeps a known Web requirement through a network failure until verified recovery", async () => {
    native.desktop = false;
    const entry = new ApplicationConnectionStore(new RootStore());
    web.login.mockRejectedValueOnce({ code: "center_protocol_incompatible", message: "fixture" });
    await entry.login("fixture"); expect(entry.update_required).toBe(true);
    web.bootstrap.mockRejectedValueOnce({ code: "center_unavailable", message: "offline" });
    await entry.recheckSession(); expect(entry.update_required).toBe(true);
    await entry.recheckSession(); expect(entry.update_required).toBe(false); expect(entry.phase).toBe("workspace");
    entry.dispose();
  });
});


it("preserves the application preference flush across identity owner replacements", async () => {
  const flush = vi.fn(async () => undefined);
  const entry = new ApplicationConnectionStore(new RootStore(), flush);
  await entry.initialize();
  await entry.signOut(false);
  const landing = entry.current as unknown as { options: { flush_application_preferences: () => Promise<void> } };
  expect(landing.options.flush_application_preferences).toBe(flush);
  await entry.login("fixture");
  const workspace = entry.current as unknown as { options: { flush_application_preferences: () => Promise<void> } };
  expect(workspace.options.flush_application_preferences).toBe(flush);
  await workspace.options.flush_application_preferences();
  expect(flush).toHaveBeenCalledOnce();
  entry.dispose();
});

const loginStatus = (patch: Partial<LocalHostStatus> = {}): LocalHostStatus => ({
  instance_id: "host-before", revision: "revision-1", mode: "enterprise", current_mode: "enterprise",
  center_url: "https://center.example.com/tenant", current_center_url: "https://center.example.com/tenant", center_id: "old-center",
  password_configured: false, listener_state: "listening", restart_required: false, error: null,
  configuration: { remote_enabled: true, scheme: "https", port: 7440, server_names: ["runtime.example.com"], tls_certificate: "cert.pem", tls_private_key: "key.pem" },
  ...patch,
});

async function loginEntry(status = loginStatus()) {
  const entry = new ApplicationConnectionStore(new RootStore());
  runInAction(() => { entry.phase = "entry"; });
  native.manage.mockReset().mockResolvedValue(status);
  native.restart.mockReset().mockResolvedValue(bootstrap("host-after"));
  await entry.openLoginOptions();
  return entry;
}

describe("login options configuration lifecycle", () => {
  it("keeps the address draft when toggling, but saves personal mode without the hidden address and never logs in after restart", async () => {
    const entry = await loginEntry();
    entry.editLoginOptions({ center_url: "https://new.example.com/path", mode: "personal" });
    entry.editLoginOptions({ mode: "enterprise" });
    expect(entry.login_options?.draft.center_url).toBe("https://new.example.com/path");
    entry.editLoginOptions({ mode: "personal" });
    entry.confirmLoginOptions();
    expect(native.manage).toHaveBeenCalledTimes(1);
    const saved = loginStatus({ revision: "revision-2", mode: "personal", center_id: undefined, restart_required: true,
      configuration: { ...loginStatus().configuration, remote_enabled: false } });
    native.manage.mockResolvedValueOnce(saved).mockResolvedValueOnce({ ...saved, instance_id: "host-after", current_mode: "personal", current_center_url: null, restart_required: false });
    await entry.applyLoginOptions();
    expect(native.manage.mock.calls[1]).toEqual([{ type: "configure", payload: { expected_revision: "revision-1", mode: "personal", clear_center_binding: true, configuration: saved.configuration } }, "host-before"]);
    expect(native.restart).toHaveBeenCalledWith({ instance_id: "host-before", revision: "revision-2" });
    expect(native.begin).toHaveBeenCalled();
    expect(native.connect).not.toHaveBeenCalled();
    expect(entry.phase).toBe("entry");
    expect(entry.mode).toBe("personal");
    expect(entry.login_options_notice).toBe("登录选项已生效");
    expect(entry.credentials_epoch).toBe(1);
    expect(entry.pending).toBe(false);
    native.manage.mockResolvedValue({ ...saved, current_mode: "personal", current_center_url: null, restart_required: false });
    await entry.openLoginOptions();
    entry.editLoginOptions({ mode: "enterprise" });
    expect(entry.login_options?.draft.center_url).toBe("https://center.example.com/tenant");
    entry.dispose();
  });

  it("does not save unchanged options and applies already saved configuration without another write", async () => {
    const entry = await loginEntry();
    entry.confirmLoginOptions();
    expect(entry.login_options?.phase).toBe("edit");
    await entry.applyLoginOptions();
    expect(native.restart).not.toHaveBeenCalled();
    entry.closeLoginOptions();
    const saved = loginStatus({ restart_required: true, current_mode: "personal", current_center_url: null });
    native.manage.mockResolvedValueOnce(saved).mockResolvedValueOnce({ ...saved, instance_id: "host-after", current_mode: "enterprise", current_center_url: saved.center_url, restart_required: false });
    await entry.openLoginOptions();
    entry.confirmLoginOptions();
    await entry.applyLoginOptions();
    expect(native.manage.mock.calls.every(([command]) => command.type === "get_status")).toBe(true);
    expect(native.restart).toHaveBeenCalledOnce();
    entry.dispose();
  });

  it.each(["invalid_request", "configuration_conflict", "desktop_update_busy"])("never restarts on explicit %s rejection", async (code) => {
    const entry = await loginEntry();
    entry.editLoginOptions({ mode: "personal" }); entry.confirmLoginOptions();
    native.manage.mockRejectedValueOnce({ code });
    await entry.applyLoginOptions();
    expect(native.restart).not.toHaveBeenCalled();
    expect(entry.login_options?.draft.mode).toBe("personal");
    expect(entry.login_options?.baseline?.revision).toBe("revision-1");
    expect(entry.pending).toBe(false);
    entry.dispose();
  });

  it("reads back a lost save response without replaying configure or automatically restarting", async () => {
    const entry = await loginEntry();
    entry.editLoginOptions({ mode: "personal" }); entry.confirmLoginOptions();
    const saved = loginStatus({ mode: "personal", center_id: undefined, revision: "revision-2", restart_required: true, configuration: { ...loginStatus().configuration, remote_enabled: false } });
    native.manage.mockRejectedValueOnce({ code: "configuration_unavailable" }).mockResolvedValueOnce(saved);
    await entry.applyLoginOptions();
    expect(native.manage.mock.calls.map(([command]) => command.type)).toEqual(["get_status", "configure", "get_status"]);
    expect(native.restart).not.toHaveBeenCalled();
    expect(entry.login_options?.recovery).toBe("restart");
    expect(entry.login_options?.message).toContain("尚未确认重启");
    entry.confirmLoginOptions();
    native.manage.mockResolvedValueOnce({ ...saved, instance_id: "host-after", current_mode: "personal", current_center_url: null, restart_required: false });
    await entry.applyLoginOptions();
    expect(native.manage.mock.calls.filter(([command]) => command.type === "configure")).toHaveLength(1);
    expect(native.restart).toHaveBeenCalledOnce();
    entry.dispose();
  });

  it("checks a newly started instance after read failure without restarting it again", async () => {
    const saved = loginStatus({ restart_required: true });
    const entry = await loginEntry(saved);
    entry.confirmLoginOptions();
    native.manage.mockRejectedValueOnce(new Error("read unavailable"));
    await entry.applyLoginOptions();
    expect(entry.login_options?.recovery).toBe("check_applied");
    native.manage.mockResolvedValueOnce({ ...saved, instance_id: "host-after", restart_required: false });
    await entry.checkLoginOptions();
    expect(native.manage).toHaveBeenLastCalledWith({ type: "get_status" }, "host-after");
    expect(native.restart).toHaveBeenCalledOnce();
    expect(entry.login_options_notice).toBe("登录选项已生效");
    entry.dispose();
  });

  it.each(["runtime_stop_failed", "runtime_startup_failed", "configuration_conflict", "desktop_update_busy"])("does not replay a saved write or bootstrap another Host after %s", async (code) => {
    const saved = loginStatus({ restart_required: true });
    const entry = await loginEntry(saved);
    const starts = native.local.mock.calls.length;
    entry.confirmLoginOptions();
    native.restart.mockRejectedValueOnce({ code });
    native.manage.mockRejectedValueOnce({ code: "configuration_unavailable" });
    await entry.applyLoginOptions();
    expect(entry.login_options?.recovery).toBe("check_applied");
    expect(entry.login_options?.baseline).toEqual(saved);
    expect(entry.pending).toBe(false);
    expect(entry.updates.operation_blocked).toBe(false);
    native.manage.mockRejectedValueOnce({ code: "configuration_conflict" });
    await entry.checkLoginOptions();
    expect(native.manage).toHaveBeenLastCalledWith({ type: "get_status" }, saved.instance_id);
    expect(native.manage.mock.calls.every(([command]) => command.type === "get_status")).toBe(true);
    expect(native.local.mock.calls).toHaveLength(starts);
    expect(native.restart).toHaveBeenCalledOnce();
    expect(native.connect).not.toHaveBeenCalled();
    expect(entry.login_options_notice).toBeNull();
    entry.dispose();
  });

  it("blocks duplicate writes, login, target changes and updates while saving", async () => {
    const entry = await loginEntry();
    entry.editLoginOptions({ mode: "personal" }); entry.confirmLoginOptions();
    const response = deferred<LocalHostStatus>();
    native.manage.mockReturnValueOnce(response.promise);
    const applying = entry.applyLoginOptions();
    await entry.applyLoginOptions();
    entry.chooseTarget("remote"); entry.closeLoginOptions();
    await entry.connectDesktop(null, "", false);
    await entry.updates.start();
    expect(entry.target).toBe("local");
    expect(native.connect).not.toHaveBeenCalled();
    expect(update.start).not.toHaveBeenCalled();
    expect(native.manage.mock.calls.filter(([command]) => command.type === "configure")).toHaveLength(1);
    response.resolve(loginStatus());
    await applying;
    expect(native.restart).not.toHaveBeenCalled();
    entry.dispose();
  });

  it("discards late reads after closing or switching the target", async () => {
    const entry = await loginEntry();
    entry.closeLoginOptions();
    const response = deferred<LocalHostStatus>();
    native.manage.mockReturnValueOnce(response.promise);
    const opening = entry.openLoginOptions();
    await Promise.resolve();
    entry.chooseTarget("remote");
    response.resolve(loginStatus()); await opening;
    expect(entry.login_options).toBeNull();
    expect(entry.target).toBe("remote");
    entry.dispose();
  });

  it("closes workspace settings on disconnect and does not reopen them on ordinary recovery", async () => {
    const entry = new ApplicationConnectionStore(new RootStore());
    await entry.initialize(); await entry.connectDesktop(null, "", false);
    const root = entry.current!;
    vi.mocked(root.settings.close).mockClear();
    runInAction(() => { root.connection.state = "disconnected"; });
    expect(entry.phase).toBe("entry");
    expect(root.settings.close).toHaveBeenCalled();
    runInAction(() => { root.connection.state = "connected"; });
    expect(entry.phase).toBe("workspace");
    expect(root.settings.open).not.toHaveBeenCalled();
    entry.dispose();
  });
});
