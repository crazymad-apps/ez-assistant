import { beforeEach, describe, expect, it, vi } from "vitest";
import { observable, runInAction } from "mobx";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";

const native = vi.hoisted(() => ({
  desktop: true,
  local: vi.fn<() => Promise<RuntimeBootstrap>>(), begin: vi.fn<() => Promise<string>>(),
  connect: vi.fn<(binding: string, origin: string | null, password: string, remember: boolean) => Promise<{ bootstrap: RuntimeBootstrap; warning: string | null }>>(),
  activate: vi.fn(), clear: vi.fn(),
}));
const web = vi.hoisted(() => ({ login: vi.fn(), bootstrap: vi.fn(), logout: vi.fn(), token: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ isTauri: () => native.desktop }));
vi.mock("../../src/runtime-client/webLogin", () => ({
  loginWeb: web.login, bootstrapWebRuntime: web.bootstrap, logoutWeb: web.logout, takeWebLoginToken: web.token,
  WebLoginError: class extends Error {},
}));
vi.mock("../../src/native-bridge/runtimeBootstrap", () => ({ bootstrapRuntime: native.local }));
vi.mock("../../src/native-bridge/runtimeConnection", () => ({ beginRuntimeConnection: native.begin, connectRuntimeTarget: native.connect, activateRuntimeBinding: native.activate, clearRuntimeBinding: native.clear, openRuntimeWeb: vi.fn() }));
vi.mock("../../src/stores/RootStore", () => ({ RootStore: class {
  connection = observable({ markDisconnected: vi.fn(), state: "disconnected", last_error_code: null as string | null, error_message: null as string | null, address: "" });
  settings = { open: vi.fn(), close: vi.fn(), showNotice: vi.fn() };
  dispose = vi.fn();
  connect = vi.fn(async (bootstrap: RuntimeBootstrap) => { runInAction(() => { this.connection.state = "connected"; this.connection.address = bootstrap.base_url; }); });
} }));
import { RootStore } from "../../src/stores/RootStore";
import { ApplicationConnectionStore } from "../../src/features/runtime-access/ApplicationConnectionStore";

const bootstrap = (origin: string): RuntimeBootstrap => ({ base_url: origin, instance_id: origin, access_token: "fixture", capabilities: { min_compatible_version: "0.25.2", runtime_version: "0.25.2", max_command_bytes: 1048576, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["web_login"] }, started_runtime: false });
const deferred = <T>() => { let resolve!: (value: T) => void; const promise = new Promise<T>((done) => { resolve = done; }); return { promise, resolve }; };
beforeEach(() => { vi.clearAllMocks(); native.desktop = true; native.local.mockResolvedValue(bootstrap("http://local")); native.begin.mockResolvedValue("binding"); native.connect.mockImplementation(async (_binding, origin) => ({ bootstrap: bootstrap(origin ?? "http://local"), warning: null })); web.login.mockResolvedValue(undefined); web.logout.mockResolvedValue(undefined); web.token.mockReturnValue(null); web.bootstrap.mockResolvedValue(bootstrap("http://web")); });

describe("Desktop connection entry and switching", () => {
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
  it("switches to a fresh projection and keeps failures in settings instead of returning to entry", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    await entry.connectDesktop(null, "", false); const previous = entry.current;
    native.connect.mockRejectedValueOnce({ code: "authentication_required", message: "密码错误" });
    await entry.connectDesktop("http://remote", "wrong", false);
    expect(previous?.dispose).toHaveBeenCalled();
    expect(entry.current).not.toBe(previous);
    expect(entry.phase).toBe("workspace");
    expect(entry.error).toBe("密码错误");
    expect(entry.current?.settings.open).toHaveBeenCalledWith("runtime_connection");
    expect(native.connect).toHaveBeenLastCalledWith("binding", "http://remote", "wrong", false);
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
  it("expired remote login clears business projection and asks for password in settings", async () => {
    const entry = new ApplicationConnectionStore(new RootStore()); await entry.initialize();
    await entry.connectDesktop("http://remote", "password", false); const previous = entry.current!;
    runInAction(() => { previous.connection.last_error_code = "authentication_required"; });
    expect(previous.dispose).toHaveBeenCalled();
    expect(entry.phase).toBe("workspace");
    expect(entry.current).not.toBe(previous);
    expect(entry.current?.settings.open).toHaveBeenCalledWith("runtime_connection");
    entry.dispose();
  });
});

describe("Web login lifecycle", () => {
  beforeEach(() => { native.desktop = false; });

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
