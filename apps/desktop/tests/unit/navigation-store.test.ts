import { describe, expect, it } from "vitest";
import { ConversationSearchStore } from "../../src/stores/ConversationSearchStore";
import { NavigationStore } from "../../src/stores/NavigationStore";

describe("NavigationStore", () => {
  it("removes a deleted owner without losing other owners' reading history", () => {
    const store = new NavigationStore(); store.selectSession("a");
    store.updateReadingPosition({ following: false, top: 42 }, 1);
    store.selectSession("deleted"); store.clearConversationLocations("deleted"); store.selectSession(null, false);
    store.selectSession("b");
    expect(store.goBack()?.reading?.top).toBe(42);
  });

  it("keeps independent main, child and reasoning intent until identity reset", () => {
    const store = new NavigationStore(); store.selectSession("s");
    store.updateReadingPosition({ following: false, top: 300, anchor: { message_id: "m", offset: -20 } }, 1);
    store.updateReadingPosition({ following: false, top: 120 }, 1, "reasoning");
    store.openChildTask("child"); store.updateReadingPosition({ following: true, top: 900 }, 1);
    store.closeChildTask();
    expect(store.current_conversation_location?.reading?.top).toBe(300);
    expect(store.current_conversation_location?.reasoning?.reasoning.top).toBe(120);
    store.openChildTask("child"); expect(store.current_conversation_location?.reading?.following).toBe(true);
    store.clearConversationLocations(); expect(store.current_conversation_location).toBeNull();
  });

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

  it("keeps the left sidebar visible and restores the proportional right sidebar after widening", () => {
    const store = new NavigationStore();
    store.setViewportWidth(950);

    expect(store.left_sidebar_open).toBe(true);
    expect(store.right_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);

    store.toggleRightSidebar();
    expect(store.left_sidebar_open).toBe(true);
    expect(store.right_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);

    store.setViewportWidth(1400);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_width).toBe(426);
    store.setViewportWidth(1600);
    expect(store.effective_left_sidebar_width).toBe(286);
    expect(store.effective_right_sidebar_width).toBe(502);
    store.setViewportWidth(1200);
    expect(store.effective_right_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_width).toBe(349);
  });

  it("clamps resized widths without treating a closed sidebar as zero preference", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2000);
    store.setSidebarWidth("left", 999);
    store.setSidebarWidth("right", 999);
    expect(store.left_sidebar_width).toBe(420);
    expect(store.effective_right_sidebar_width).toBeGreaterThanOrEqual(998);
    expect(store.effective_right_sidebar_width).toBeLessThanOrEqual(999);

    store.toggleLeftSidebar();
    expect(store.left_sidebar_width).toBe(420);
    store.toggleLeftSidebar();
    expect(store.effective_left_sidebar_width).toBe(420);

    store.resetSidebarLayout();
    expect(store.left_sidebar_width).toBe(286);
    expect(store.right_sidebar_width).toBe(380);
  });

  it("keeps the right sidebar ratio while hiding it only below the combined minimum", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2000);
    store.setSidebarWidth("right", 760);

    store.setViewportWidth(1000);

    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);
    expect(store.effective_left_sidebar_width).toBe(286);
    expect(store.effective_right_sidebar_width).toBe(0);
    expect(store.right_sidebar_width).toBeGreaterThan(0);

    store.setViewportWidth(2000);
    expect(store.effective_left_sidebar_width).toBe(286);
    expect(store.effective_right_sidebar_width).toBe(760);
  });

  it("keeps the left sidebar at its preferred width after the right sidebar reaches its minimum", () => {
    const store = new NavigationStore();

    store.setViewportWidth(1_106);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_width).toBe(320);

    store.setViewportWidth(1_105);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);

    store.setViewportWidth(785);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_right_sidebar_open).toBe(false);
    expect(store.effective_left_sidebar_width).toBe(286);
  });

  it("lets a manually widened left sidebar keep its width while the right sidebar yields", () => {
    const store = new NavigationStore();
    store.setViewportWidth(1_280);
    store.setSidebarWidth("left", 420);
    expect(store.effective_left_sidebar_width).toBe(420);
    expect(store.effective_right_sidebar_open).toBe(true);

    store.setViewportWidth(920);
    expect(store.effective_left_sidebar_width).toBe(420);
    expect(store.effective_right_sidebar_open).toBe(false);
  });

  it("does not trade the left sidebar away when the user requests the right sidebar in a narrow window", () => {
    const store = new NavigationStore();
    store.setViewportWidth(1_105);
    store.toggleRightSidebar();
    expect(store.effective_right_sidebar_open).toBe(false);
    expect(store.effective_left_sidebar_width).toBe(286);
    store.setViewportWidth(1_106);
    expect(store.effective_right_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_open).toBe(true);
    expect(store.effective_left_sidebar_width).toBe(286);
  });

  it("uses the remaining viewport as the right sidebar maximum without a fixed cap", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2_400);

    expect(store.right_sidebar_current_max_width).toBe(1_614);
    store.setSidebarWidth("right", 1_500);
    expect(store.effective_right_sidebar_width).toBeGreaterThanOrEqual(1_499);
    expect(store.effective_right_sidebar_width).toBeLessThanOrEqual(1_500);
    expect(store.left_sidebar_width).toBe(286);

    store.setSidebarWidth("right", 2_000);
    expect(store.effective_right_sidebar_width).toBe(1_614);
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
    expect(store.right_sidebar_width).toBe(9_999);
    expect(store.effective_right_sidebar_width).toBe(600);
  });

  it("restores a minimum-width resource rail from its proportional preference", () => {
    const store = new NavigationStore();
    store.setViewportWidth(2_400);
    store.setSidebarWidth("right", 320);
    const preference = store.right_sidebar_width;
    expect(preference).toBeLessThan(320);

    const restored = new NavigationStore();
    restored.setViewportWidth(2_400);
    restored.applyPreferences({
      left_sidebar_open: true,
      right_sidebar_open: true,
      left_sidebar_width: 286,
      right_sidebar_width: preference,
      expanded_workspace_ids: null,
    });
    expect(restored.effective_right_sidebar_width).toBe(320);
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
