import { afterEach, describe, expect, it, vi } from "vitest";
import { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
const bootstrap: RuntimeBootstrap = {
  base_url: "http://127.0.0.1:9000", access_token: "fixture", instance_id: "one", started_runtime: false,
  capabilities: { protocol_version: 3, runtime_version: "0.25.1", max_command_bytes: 1000, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["startup_diagnostics"] },
};
const health = (status: string, error: string | null = null) => new Response(JSON.stringify({ status, stage: "database_migration", error, database_version: null, target_version: "0.25.1" }));
afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });
describe("authenticated startup observation", () => {
  it("polls only health until ready and forwards the current stage", async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn().mockResolvedValueOnce(health("starting")).mockResolvedValueOnce(health("ready"));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap), receive = vi.fn();
    const waiting = client.waitUntilReady(receive);
    await vi.advanceTimersByTimeAsync(1001); await waiting;
    expect(receive.mock.calls.map(([state]) => state.status)).toEqual(["starting", "ready"]);
    expect(fetcher.mock.calls.every(([url]) => url.endsWith("/health"))).toBe(true);
    expect(fetcher.mock.calls[0][1].headers.get("Authorization")).toBe("Bearer fixture");
    client.dispose();
  });
  it("does not retry failed initialization", async () => {
    const fetcher = vi.fn().mockResolvedValue(health("unavailable", "migration_failed"));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    await expect(client.waitUntilReady(vi.fn())).rejects.toMatchObject({ code: "runtime_startup_failed" });
    expect(fetcher).toHaveBeenCalledTimes(1); client.dispose();
  });
  it("cancels the poll delay when the connection is released", async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn().mockResolvedValue(health("starting"));
    vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient(bootstrap);
    const waiting = client.waitUntilReady(vi.fn());
    const rejected = expect(waiting).rejects.toMatchObject({ name: "AbortError" });
    await vi.advanceTimersByTimeAsync(1); client.dispose(); await rejected;
    await vi.advanceTimersByTimeAsync(3000); expect(fetcher).toHaveBeenCalledTimes(1);
  });
  it("rejects old Host protocol before making a request", async () => {
    const fetcher = vi.fn(); vi.stubGlobal("fetch", fetcher);
    const client = new RuntimeClient({ ...bootstrap, capabilities: { ...bootstrap.capabilities, protocol_version: 2 } });
    await expect(client.waitUntilReady(vi.fn())).rejects.toMatchObject({ code: "component_mismatch" });
    expect(fetcher).not.toHaveBeenCalled(); client.dispose();
  });
});
