import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConversationPage, SessionViewSnapshot } from "@ez-assistant/protocol";
import { RootStore } from "../../src/stores/RootStore";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { RuntimeClient, RuntimeClientError } from "../../src/runtime-client/RuntimeClient";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";

const page = (ids: string[], generation = 1): ConversationPage => ({
  owner: { type: "main_session", session_id: "s" }, generation,
  items: ids.map(message_id => ({ type: "user", message_id, input_id: null, text: message_id,
    created_at_ms: 0, attachment_ids: [], quotes: [], source: { type: "user" } })),
  previous_cursor: ids[0] === "m1" ? null : "cursor", has_more: ids[0] !== "m1",
});
const snapshot = (page: ConversationPage, sequence = 1) => ({ observed_sequence: sequence, value: {
  session: { session_id: "s" }, conversation: page,
} as SessionViewSnapshot });
const response = (value: ConversationPage) => ({ type: "list_conversation_page" as const,
  payload: { snapshot: { observed_sequence: 1, value } } });
function deferred<T>() { let resolve!: (value: T) => void; let reject!: (error: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; }); return { promise, resolve, reject }; }
function fixture() {
  const store = new RootStore();
  const client = new RuntimeClient({ base_url: "http://fixture", instance_id: "same-id", access_token: "test" } as RuntimeBootstrap);
  const getter = vi.spyOn(RuntimeLifecycleCoordinator.prototype, "client", "get").mockReturnValue(client);
  store.projection.applySessionSnapshot(snapshot(page(["m3", "m4"])));
  return { store, client, getter };
}
afterEach(() => vi.restoreAllMocks());

describe("conversation page request ownership", () => {
  it("abandons anchor recovery silently when the reader navigates away", async () => {
    const { store, client } = fixture(); store.navigation.selectSession("s");
    const pending = deferred<ReturnType<typeof response>>();
    const command = vi.spyOn(client, "command").mockReturnValue(pending.promise);
    const load = vi.spyOn(RuntimeLifecycleCoordinator.prototype, "loadSession");
    const recovering = store.restoreReadingAnchor("s", null, "missing");
    store.navigation.selectSession("another"); pending.resolve(response(page(["m1"])));
    expect(await recovering).toBe(false); expect(command).toHaveBeenCalledOnce();
    expect(load).not.toHaveBeenCalled();
    expect(store.interaction_error).toBeNull();
  });

  it("accepts one delayed old page after a live tail snapshot without consuming its watermark", async () => {
    const { store, client } = fixture(); const pending = deferred<ReturnType<typeof response>>();
    const command = vi.spyOn(client, "command").mockReturnValue(pending.promise);
    const loading = store.loadPreviousConversationPage("s");
    store.projection.applySessionSnapshot(snapshot(page(["m4", "m5"]), 20));
    expect(await store.loadPreviousConversationPage("s")).toBe(false); expect(command).toHaveBeenCalledOnce();
    pending.resolve(response(page(["m1", "m2"]))); expect(await loading).toBe(true);
    expect(store.projection.conversation_histories.get("s")?.items.map(item => item.message_id)).toEqual(["m1", "m2", "m3", "m4", "m5"]);
    expect(store.projection.observed_sequence).toBe(0);
  });
  it("does not let a late old client clear the new client's pending load even with the same instance ID", async () => {
    const { store, client, getter } = fixture(); const old = deferred<ReturnType<typeof response>>();
    vi.spyOn(client, "command").mockReturnValue(old.promise);
    const first = store.loadPreviousConversationPage("s");
    const replacement = new RuntimeClient({ base_url: "http://fixture", instance_id: "same-id", access_token: "new" } as RuntimeBootstrap);
    getter.mockReturnValue(replacement); store.projection.resetForInstance();
    store.projection.applySessionSnapshot(snapshot(page(["m3", "m4"])));
    const fresh = deferred<ReturnType<typeof response>>(); vi.spyOn(replacement, "command").mockReturnValue(fresh.promise);
    const second = store.loadPreviousConversationPage("s");
    old.reject(new RuntimeClientError("snapshot_busy", "old failure")); expect(await first).toBe(false);
    expect(store.projection.conversation_histories.get("s")?.is_loading_previous).toBe(true);
    fresh.resolve(response(page(["m1", "m2"]))); expect(await second).toBe(true);
  });
  it("rejects a generation change during the request without joining old content", async () => {
    const { store, client } = fixture(); const pending = deferred<ReturnType<typeof response>>();
    vi.spyOn(client, "command").mockReturnValue(pending.promise); const loading = store.loadPreviousConversationPage("s");
    store.projection.applySessionSnapshot(snapshot(page(["rewritten"], 2), 20));
    pending.resolve(response(page(["m1", "m2"]))); expect(await loading).toBe(false);
    expect(store.projection.conversation_histories.get("s")?.items[0].message_id).toBe("rewritten");
  });
  it("preserves history after failure and only retries on another explicit request", async () => {
    const { store, client } = fixture(); const command = vi.spyOn(client, "command").mockRejectedValueOnce(new RuntimeClientError("snapshot_busy", "busy"));
    expect(await store.loadPreviousConversationPage("s")).toBe(false);
    expect(store.projection.conversation_histories.get("s")).toMatchObject({ is_loading_previous: false, load_error: "历史正在更新，请重试。" });
    expect(command).toHaveBeenCalledOnce();
    command.mockResolvedValueOnce(response(page(["m1", "m2"])));
    expect(await store.loadPreviousConversationPage("s")).toBe(true);
  });
});
