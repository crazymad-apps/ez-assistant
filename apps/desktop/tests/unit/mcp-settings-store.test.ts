import { runInAction } from "mobx";
import { describe, expect, it } from "vitest";
import { SettingsStore } from "../../src/stores/SettingsStore";
import { McpSettingsStore } from "../../src/features/settings/SettingsDialog/McpSettingsPage/store";
import { serverDraft, validateMcpDraft } from "../../src/features/settings/SettingsDialog/McpSettingsPage/draft";
import type { RuntimeClient } from "../../src/runtime-client/RuntimeClient";
import type { McpServerDraft, RuntimeCommand } from "../../src/generated/assistant-protocol";

function deferred() {
  let resolve!: (value: unknown) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<unknown>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function client(command: (request: RuntimeCommand) => Promise<unknown>): RuntimeClient {
  return { command } as unknown as RuntimeClient;
}
const snapshot = { revision: "r1", needs_refresh: false, servers: [], diagnostics: [] };

describe("MCP settings request ownership", () => {
  it.each([undefined, 1, 30_000, 300_000, 1_800_000, Number.MAX_SAFE_INTEGER])("accepts a tool timeout override of %s independently from the default", (timeout) => {
    const draft = timeoutDraft(timeout);
    expect(validateMcpDraft(draft)).toBeNull();
  });
  it.each([0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1, Number.NaN, Number.POSITIVE_INFINITY])("rejects unsafe tool timeout %s", (timeout) => {
    expect(validateMcpDraft(timeoutDraft(timeout))).not.toBeNull();
  });
  it.each(["navigation", "close"])("ignores a late read failure after %s", async mode => {
    const pending = deferred();
    const runtime = client(() => pending.promise);
    const settings = new SettingsStore({ get_client: () => runtime, get_permission_context: () => ({ session_id: null, workspace_id: null }), refresh_application: async () => {} });
    const reading = settings.mcp.load();
    if (mode === "close") settings.close(); else settings.selectPage("models");
    pending.reject(new Error("secret error body"));
    await reading;
    expect(settings.error_message).toBeNull();
    expect(settings.mcp.error_message).toBeNull();
    expect(settings.mcp.loading).toBe(false);
  });

  it("does not apply a response from the previous Runtime client", async () => {
    const old = deferred();
    let runtime = client(() => old.promise);
    const store = new McpSettingsStore(() => runtime);
    const reading = store.load();
    runtime = client(async () => ({ type: "get_mcp_configuration", payload: { snapshot: { ...snapshot, revision: "new" } } }));
    await store.load();
    old.resolve({ type: "get_mcp_configuration", payload: { snapshot } });
    await reading;
    expect(store.configuration?.revision).toBe("new");
    expect(store.stale).toBe(false);
  });

  it("does not let a previous view mutation overwrite a new request or clear its pending state", async () => {
    const old = deferred();
    const next = deferred();
    let calls = 0;
    const runtime = client(() => (++calls === 1 ? old : next).promise);
    const store = new McpSettingsStore(() => runtime);
    const first = store.mutate("r1", { type: "remove", payload: { server_key: "old" } });
    store.deactivate();
    runInAction(() => { store.stale = false; });
    const second = store.mutate("r2", { type: "remove", payload: { server_key: "new" } });
    old.resolve({ type: "mutate_mcp_configuration", payload: { snapshot } });
    expect(await first).toBe(false);
    expect(store.configuration).toBeNull();
    expect(store.pending_action).toBe("save");
    next.resolve({ type: "mutate_mcp_configuration", payload: { snapshot: { ...snapshot, revision: "r3" } } });
    expect(await second).toBe(true);
    expect(store.configuration?.revision).toBe("r3");
    expect(store.pending_action).toBeNull();
  });

  it("sends test cancellation to the originating client after reconnect", async () => {
    const pending = deferred();
    const old_commands: RuntimeCommand[] = [];
    const new_commands: RuntimeCommand[] = [];
    let runtime = client(async request => { old_commands.push(request); return request.type === "test_mcp_server" ? pending.promise : {}; });
    const store = new McpSettingsStore(() => runtime);
    const testing = store.test(serverDraft(null));
    runtime = client(async request => { new_commands.push(request); return {}; });
    store.deactivate();
    pending.resolve({ type: "test_mcp_server", payload: { outcome: "success", stage: "complete", elapsed_ms: 1, tool_count: 1 } });
    await testing;
    expect(old_commands.map(item => item.type)).toEqual(["test_mcp_server", "cancel_mcp_server_test"]);
    expect(new_commands).toEqual([]);
    expect(store.test_result).toBeNull();
  });
});

function timeoutDraft(timeout: number | undefined): McpServerDraft {
  return { ...serverDraft(null), server_key: "fixture", tool_timeout_ms: timeout,
    transport: { type: "stdio", payload: { command: { mode: "replace", value: "fixture" },
      args: { mode: "replace", value: [] }, cwd: { mode: "remove" }, environment: {} } } };
}
