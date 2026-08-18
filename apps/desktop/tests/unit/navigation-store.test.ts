import { describe, expect, it } from "vitest";
import { ConversationSearchStore } from "../../src/stores/ConversationSearchStore";
import { NavigationStore } from "../../src/stores/NavigationStore";

describe("NavigationStore", () => {
  it("restores conversation source locations with browser-style back and forward navigation", () => {
    const store = new NavigationStore();
    store.selectSession("session-1");
    store.updateCurrentScrollOffset(320);
    store.navigateTo({
      session_id: "session-2",
      child_task_id: "child-1",
      anchor_message_id: "message-8",
      scroll_offset: null,
    });

    expect(store.can_go_back).toBe(true);
    expect(store.goBack()).toMatchObject({ session_id: "session-1", scroll_offset: 320 });
    expect(store.selected_session_id).toBe("session-1");
    expect(store.goForward()).toMatchObject({
      session_id: "session-2",
      child_task_id: "child-1",
      anchor_message_id: "message-8",
    });
    expect(store.selected_child_task_id).toBe("child-1");
  });

  it("drops the forward branch after navigating from an older history entry", () => {
    const store = new NavigationStore();
    store.selectSession("session-1");
    store.selectSession("session-2");
    store.goBack();
    store.selectSession("session-3");

    expect(store.can_go_forward).toBe(false);
    expect(store.conversation_history.map((item) => item.session_id)).toEqual([
      "session-1",
      "session-3",
    ]);
  });
});

describe("ConversationSearchStore", () => {
  it("clears stale results immediately when switching scope", () => {
    const store = new ConversationSearchStore();
    store.applySearch([], 40, true, true);
    store.setScope("global");

    expect(store.scope).toBe("global");
    expect(store.status).toBe("idle");
    expect(store.next_offset).toBeNull();
    expect(store.partial).toBe(false);
  });

  it("clears transient results and restores the default scope", () => {
    const store = new ConversationSearchStore();
    store.setScope("global");
    store.setQuery("历史消息");
    store.beginSearch(true);
    store.failSearch("failed");
    store.reset();

    expect(store.scope).toBe("session");
    expect(store.query).toBe("");
    expect(store.status).toBe("idle");
    expect(store.error).toBeNull();
  });
});
