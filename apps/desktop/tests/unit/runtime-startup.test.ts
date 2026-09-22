import { afterEach, describe, expect, it, vi } from "vitest";
import { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
import { startupMessage } from "../../src/runtime-client/startupStatus";
const bootstrap: RuntimeBootstrap = {
  base_url: "http://127.0.0.1:9000", access_token: "fixture", instance_id: "one", started_runtime: false,
  capabilities: { mode: "personal", min_compatible_version: "0.27.0", runtime_version: "0.27.0", max_command_bytes: 1000, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["startup_diagnostics"] },
};
const health = (status: string, error: string | null = null) => new Response(JSON.stringify({ status, stage: "database_migration", error, database_version: null, target_version: "0.25.1" }));
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });
describe("authenticated startup observation", () => {
  it("uses explicit Bearer without ambient cookies and same-origin cookies otherwise", async () => {
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve(health("ready")));
    vi.stubGlobal("fetch", fetcher);
    for (const token of ["fixture", ""]) {
      const client = new RuntimeClient({ ...bootstrap, access_token: token });
      await client.waitUntilReady(() => undefined);
      const init = fetcher.mock.lastCall?.[1];
      expect(init.credentials).toBe(token ? "omit" : "same-origin");
      expect(init.headers.get("Authorization")).toBe(token ? `Bearer ${token}` : null);
      client.dispose();
    }
  });
  it("shows the required Host software version without implying automatic recovery", () => {
    expect(startupMessage({
      status: "unavailable", stage: "database_check", error: "database_host_too_old",
      database_version: null, target_version: "0.25.2", min_compatible_host_version: "0.25.3",
    })).toBe("数据库要求 Host 至少为 0.25.3，当前为 0.25.2。未继续初始化，请使用兼容版本。");
  });
  it("waits for one blocking readiness request without polling", async () => {
    let finish!: (response: Response) => void;
    const fetcher = vi.fn(() => new Promise<Response>((resolve) => { finish = resolve; }));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap), receive = vi.fn();
    const waiting = client.waitUntilReady(receive);
    await Promise.resolve();
    expect(receive).not.toHaveBeenCalled();
    finish(new Response(null, { status: 204 })); await waiting;
    expect(receive).toHaveBeenCalledWith(expect.objectContaining({ status: "ready" }));
    expect(fetcher).toHaveBeenCalledOnce();
    expect(fetcher).toHaveBeenCalledWith(expect.stringContaining("/runtime/ensure-ready"), expect.objectContaining({ method: "POST" }));
    client.dispose();
  });
  it("does not retry failed initialization", async () => {
    const fetcher = vi.fn().mockResolvedValue(Response.json({ error: { code: "storage_unavailable", message: "初始化失败" } }, { status: 503 }));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    await expect(client.waitUntilReady(vi.fn())).rejects.toMatchObject({ code: "storage_unavailable" });
    expect(fetcher).toHaveBeenCalledTimes(1); client.dispose();
  });
  it("cancels the blocking wait when the connection is released", async () => {
    const fetcher = vi.fn((_url, init) => new Promise<Response>((_resolve, reject) => {
      init.signal.addEventListener("abort", () => reject(new DOMException("closed", "AbortError")));
    }));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    const waiting = client.waitUntilReady(vi.fn());
    client.dispose();
    await expect(waiting).rejects.toMatchObject({ name: "AbortError" });
    expect(fetcher).toHaveBeenCalledOnce();
  });
  it("rejects old Host software before making a request", async () => {
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient({ ...bootstrap, capabilities: { ...bootstrap.capabilities, runtime_version: "0.25.1", min_compatible_version: "0.25.1" } });
    await expect(client.waitUntilReady(vi.fn())).rejects.toMatchObject({ code: "component_mismatch" });
    expect(fetcher).not.toHaveBeenCalled(); client.dispose();
  });
});

describe("event connection compatibility", () => {
  const listener = { onEvent: vi.fn(), onDeviceGatewayEvent: vi.fn(), onGap: vi.fn() };
  it("rechecks the current Host on every event connection and rejects a changed floor", async () => {
    const fetcher = vi.fn()
      .mockResolvedValueOnce(Response.json(bootstrap.capabilities))
      .mockResolvedValueOnce(new Response(""))
      .mockResolvedValueOnce(Response.json({ ...bootstrap.capabilities, runtime_version: "99.0.0", min_compatible_version: "99.0.0" }));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    await (await client.connectEvents(listener, new AbortController().signal)).closed;
    await expect(client.connectEvents(listener, new AbortController().signal)).rejects.toMatchObject({ code: "client_too_old" });
    expect(fetcher.mock.calls.map(([url]) => new URL(url).pathname)).toEqual(["/capabilities", "/events", "/capabilities"]);
    expect(fetcher.mock.calls.every(([, init]) => init.headers.get("x-ez-client-version") === "0.27.0" && init.headers.get("x-ez-min-compatible-version") === "0.27.0")).toBe(true);
    client.dispose();
  });
  it("preserves a compatibility failure between capabilities and SSE admission", async () => {
    const fetcher = vi.fn().mockResolvedValueOnce(Response.json(bootstrap.capabilities))
      .mockResolvedValueOnce(Response.json({ error: { code: "host_too_old" } }, { status: 409 }));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    await expect(client.connectEvents(listener, new AbortController().signal)).rejects.toMatchObject({ code: "host_too_old", message: "Host 低于客户端的最低兼容版本，请更新 Host。" });
    client.dispose();
  });
});
