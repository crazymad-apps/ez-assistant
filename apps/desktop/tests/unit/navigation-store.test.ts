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

  it("selects a new-session draft without adding a fake conversation history entry", () => {
    const store = new NavigationStore();
    store.selectSession("session-1");
    const before = [...store.conversation_history];

    store.selectDraft("workspace:workspace-1");

    expect(store.selected_session_id).toBeNull();
    expect(store.selected_draft_key).toBe("workspace:workspace-1");
    expect([...store.conversation_history]).toEqual(before);
    store.selectSession("session-1");
    expect(store.selected_draft_key).toBeNull();
  });

  it("keeps preferred sidebar state separate from narrow-window effective visibility", () => {
    const store = new NavigationStore();
    store.setViewportWidth(900);

    expect(store.left_sidebar_open).toBe(true);
    expect(store.right_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);

    store.toggleRightSidebar();
    expect(store.left_sidebar_open).toBe(true);
    expect(store.right_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_open).toBe(false);
    expect(store.effective_right_sidebar_open).toBe(true);

    store.setViewportWidth(1400);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(true);
  });

  it("clamps resized widths without treating a closed sidebar as zero preference", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2000);
    store.setSidebarWidth("left", 999);
    store.setSidebarWidth("right", 999);
    expect(store.left_sidebar_width).toBe(420);
    expect(store.right_sidebar_width).toBe(999);

    store.toggleLeftSidebar();
    expect(store.left_sidebar_width).toBe(420);
    store.toggleLeftSidebar();
    expect(store.effective_left_sidebar_width).toBe(420);

    store.resetSidebarLayout();
    expect(store.left_sidebar_width).toBe(286);
    expect(store.right_sidebar_width).toBe(380);
  });

  it("keeps preferred sidebar widths fixed and hides the lower-priority sidebar when they no longer fit", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2000);
    store.setSidebarWidth("right", 760);

    store.setViewportWidth(1000);

    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);
    expect(store.effective_left_sidebar_width).toBe(286);
    expect(store.effective_right_sidebar_width).toBe(0);
    expect(store.right_sidebar_width).toBe(760);

    store.setViewportWidth(2000);
    expect(store.effective_left_sidebar_width).toBe(286);
    expect(store.effective_right_sidebar_width).toBe(760);
  });

  it("hides the right sidebar first and then the left at fixed-width thresholds", () => {
    const store = new NavigationStore();

    store.setViewportWidth(1_166);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(true);

    store.setViewportWidth(1_165);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);

    store.setViewportWidth(785);
    expect(store.effective_left_sidebar_open).toBe(false);
    expect(store.effective_right_sidebar_open).toBe(false);
  });

  it("uses the remaining viewport as the right sidebar maximum without a fixed cap", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2_400);

    expect(store.right_sidebar_current_max_width).toBe(1_614);
    store.setSidebarWidth("right", 1_500);
    expect(store.right_sidebar_width).toBe(1_500);
    expect(store.left_sidebar_width).toBe(286);

    store.setSidebarWidth("right", 2_000);
    expect(store.right_sidebar_width).toBe(1_614);
    expect(store.left_sidebar_width).toBe(286);
  });

  it("clamps restored right sidebar preferences to the current viewport", () => {
    const store = new NavigationStore();
    store.setViewportWidth(1_400);
    store.applyPreferences({
      left_sidebar_open: true,
      right_sidebar_open: true,
      left_sidebar_width: 300,
      right_sidebar_width: 9_999,
      expanded_workspace_ids: null,
    });

    expect(store.left_sidebar_width).toBe(300);
    expect(store.right_sidebar_width).toBe(600);
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
