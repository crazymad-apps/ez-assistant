import type { ConversationItem, ConversationNavigationPage, RuntimeCommand } from "@ez-assistant/protocol";
import { observable, runInAction } from "mobx";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ConversationNavigationStore } from "../../src/stores/ConversationNavigationStore";
import { RuntimeClient, RuntimeClientError } from "../../src/runtime-client/RuntimeClient";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
import type { RuntimeReadQuery } from "../../src/stores/RuntimeLifecycleCoordinator";

const owner = { type: "main_session" as const, session_id: "navigation" };
const client = () => new RuntimeClient({ base_url: "http://fixture", instance_id: "same-instance", access_token: "fixture" } as RuntimeBootstrap);
const stores: ConversationNavigationStore[] = [];
afterEach(() => { for (const store of stores.splice(0)) store.dispose(); });
const snapshot = <T>(value: T) => ({ payload: { snapshot: { value, observed_sequence: 20 } } });
const latest = (assistant_message_id: string | null = "reply"): ConversationNavigationPage => ({
  owner, generation: 1, items: [{ user_message_id: "user", assistant_message_id }], previous_cursor: null, next_cursor: null,
});

function fixture(history: ConversationItem[] = []) {
  const command = vi.fn<(command: RuntimeCommand) => Promise<unknown>>();
  const runtime = { readConversation: async <T>(
    run: (query: RuntimeReadQuery, signal: AbortSignal) => Promise<T>, signal = new AbortController().signal,
  ) => run(command as RuntimeReadQuery, signal) };
  const original = { owner, generation: 1, client: client(), runtime };
  const scope = observable.box<typeof original | null>(original, { deep: false });
  const store = new ConversationNavigationStore(() => scope.get(), () => history);
  stores.push(store);
  store.activate();
  return { store, scope, original, command };
}

describe("conversation navigation reads", () => {
  it("keeps at most 512 contiguous IDs and re-reads evicted pages in both directions", async () => {
    const { store, command } = fixture();
    command.mockImplementation(async request => {
      if (request.type !== "list_conversation_navigation") throw new Error("unexpected preview");
      const { cursor, limit = 128 } = request.payload;
      const end = !cursor ? 10_000 : cursor.startsWith("before:")
        ? Number(cursor.slice(7)) : Math.min(Number(cursor.slice(6)) + limit, 10_000);
      const start = cursor?.startsWith("after:") ? Number(cursor.slice(6)) : Math.max(0, end - limit);
      return snapshot({
        owner, generation: 1, previous_cursor: start ? `before:${start}` : null, next_cursor: end < 10_000 ? `after:${end}` : null,
        items: Array.from({ length: end - start }, (_, index) => ({ user_message_id: String(start + index), assistant_message_id: null })),
      });
    });
    const ids = () => store.pages.flatMap(page => page.value.items.map(item => Number(item.user_message_id)));
    await store.loadPage();
    for (let page = 1; page < Math.ceil(10_000 / 128); page++) {
      expect(await store.loadPage("older")).toBe(true);
      const values = ids();
      expect(values.length).toBeLessThanOrEqual(512);
      expect(values.at(-1)! - values[0]! + 1).toBe(values.length);
    }
    expect(ids()[0]).toBe(0);
    expect(await store.loadPage("older")).toBe(false);
    for (let page = 4; page < Math.ceil(10_000 / 128); page++) {
      expect(await store.loadPage("newer")).toBe(true);
      const values = ids();
      expect(values.length).toBeLessThanOrEqual(512);
      expect(values.at(-1)! - values[0]! + 1).toBe(values.length);
    }
    expect(ids().at(-1)).toBe(9_999);
    expect(await store.loadPage("newer")).toBe(false);
  });

  it("uses already loaded Unicode text and skips an absent assistant", async () => {
    const { store, command } = fixture([{
      type: "user", message_id: "user", inherited: false, input_id: null,
      text: "树".repeat(159) + "🪴" + "尾", attachment_ids: [], quotes: [], created_at_ms: null,
      source: { type: "user" },
    }]);
    command.mockResolvedValue(snapshot(latest(null)));
    await store.loadPage();
    await store.openPreview("user");
    expect(command).toHaveBeenCalledOnce();
    expect(store.preview).toEqual({
      user_message_id: "user", user: { status: "ready", value: { text: "树".repeat(159) + "🪴", truncated: true } }, assistant: null,
    });
  });

  it("revalidates the empty list and preserves a reader's anchor when the latest page rolls over", async () => {
    const { store, command, original } = fixture();
    const page = (ids: string[], previous_cursor: string | null = null, next_cursor: string | null = null) => snapshot({
      ...latest(null), previous_cursor, next_cursor,
      items: ids.map(user_message_id => ({ user_message_id, assistant_message_id: null })),
    });
    const committed = { type: "conversation_committed", owner } as const;
    command.mockResolvedValueOnce(page([]));
    await store.loadPage();
    command.mockResolvedValueOnce(page(["old", "recent"]));
    store.observe(committed as Parameters<typeof store.observe>[0], original.client);
    await vi.waitFor(() => expect(store.pages[0].value.items).toHaveLength(2));
    store.setVisiblePages([null], false, "old");
    command.mockResolvedValueOnce(page(["recent", "new"], "before-recent"))
      .mockResolvedValueOnce(page(["old"], null, "after-old"));
    store.observe(committed as Parameters<typeof store.observe>[0], original.client);
    await vi.waitFor(() => expect(store.pages.flatMap(value => value.value.items.map(item => item.user_message_id)))
      .toEqual(["old", "recent", "new"]));
  });

  it("keeps preview failures local and retries the failed side without re-reading the successful side", async () => {
    const { store, command } = fixture();
    command.mockResolvedValueOnce(snapshot(latest()))
      .mockResolvedValueOnce(snapshot({ owner, generation: 1, message_id: "user", text: "", truncated: false }))
      .mockRejectedValueOnce(new RuntimeClientError("snapshot_busy", "暂时繁忙"));
    await store.loadPage();
    await store.openPreview("user");
    expect(store.preview).toMatchObject({
      user: { status: "ready", value: { text: "", truncated: false } },
      assistant: { status: "error", message: "暂时繁忙" },
    });
    command.mockResolvedValueOnce(snapshot({ owner, generation: 1, message_id: "reply", text: "最终回复", truncated: false }));
    await store.openPreview("user");
    expect(store.preview?.assistant).toMatchObject({ status: "ready", value: { text: "最终回复" } });
    expect(command.mock.calls.slice(1).map(([request]) => request.type)).toEqual(Array(3).fill("get_conversation_message_preview"));
  });

  it("evicts the least recently used short preview at the 256-entry bound", async () => {
    const { store, command } = fixture();
    command.mockImplementation(async request => {
      if (request.type === "get_conversation_message_preview") {
        return snapshot({ ...request.payload, text: request.payload.message_id, truncated: false });
      }
      if (request.type !== "list_conversation_navigation") throw new Error("unexpected read");
      const end = request.payload.cursor === null ? 257 : Number(request.payload.cursor);
      const start = Math.max(0, end - 128);
      return snapshot({
        owner, generation: 1, previous_cursor: start ? String(start) : null, next_cursor: null,
        items: Array.from({ length: end - start }, (_, index) => ({
          user_message_id: String(start + index), assistant_message_id: null,
        })),
      });
    });
    await store.loadPage();
    await store.loadPage("older");
    await store.loadPage("older");
    for (let id = 0; id < 257; id++) await store.openPreview(String(id));
    const before = command.mock.calls.length;
    await store.openPreview("256");
    expect(command).toHaveBeenCalledTimes(before);
    await store.openPreview("0");
    expect(command).toHaveBeenCalledTimes(before + 1);
  });

  it("revalidates the visible page and retires the old assistant on the next Step", async () => {
    const { store, command, original } = fixture();
    let deliver!: (value: unknown) => void;
    command.mockResolvedValueOnce(snapshot(latest()))
      .mockResolvedValueOnce(snapshot({ owner, generation: 1, message_id: "user", text: "输入", truncated: false }))
      .mockReturnValueOnce(new Promise(resolve => { deliver = resolve; }))
      .mockResolvedValueOnce(snapshot(latest(null)))
      .mockResolvedValueOnce(snapshot({ owner, generation: 1, message_id: "user", text: "输入", truncated: false }));
    await store.loadPage();
    store.setVisiblePages([null]);
    const pending = store.openPreview("user");
    await vi.waitFor(() => expect(store.preview?.assistant?.status).toBe("loading"));
    store.observe({ type: "step_started", session_id: owner.session_id, run_id: "run", step: 2 }, original.client);
    expect(store.preview).toBeNull();
    await vi.waitFor(() => expect(store.preview?.user.status).toBe("ready"));
    expect(store.pages[0]?.value.items[0]?.assistant_message_id).toBeNull();
    deliver(snapshot({ owner, generation: 1, message_id: "reply", text: "过期正文", truncated: false }));
    await pending;
    expect(store.preview?.assistant).toBeNull();
    expect(command.mock.calls.filter(([request]) => request.type === "get_conversation_message_preview"
      && request.payload.message_id === "reply")).toHaveLength(1);
  });

  it("clears prior generation and connection results and ignores events while hidden", async () => {
    const { store, scope, original, command } = fixture();
    command.mockResolvedValueOnce(snapshot(latest(null)));
    await store.loadPage();
    store.suspend();
    store.observe({ type: "step_started", session_id: owner.session_id, run_id: "run", step: 2 }, original.client);
    await Promise.resolve();
    expect(command).toHaveBeenCalledOnce();
    expect(store.pages[0]?.stale).toBe(true);
    store.activate();
    let deliver!: (value: unknown) => void;
    command.mockReturnValueOnce(new Promise(resolve => { deliver = resolve; }));
    const pending = store.loadPage();
    runInAction(() => scope.set({ ...original, generation: 2, client: client() }));
    deliver(snapshot(latest()));
    expect(await pending).toBe(false);
    expect(store.pages).toEqual([]);
    expect(store.preview).toBeNull();
  });
});
