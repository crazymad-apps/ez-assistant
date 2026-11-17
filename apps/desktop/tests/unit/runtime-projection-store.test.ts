import { sidebarFixture } from "../support/sidebarFixture";
import { describe, expect, it } from "vitest";
import type {
  ApplicationSnapshot,
  ChildTaskViewSnapshot,
  ConversationItem,
  ConversationPage,
  RuntimeEventEnvelope,
  RunSnapshot,
  SessionViewSnapshot,
} from "@ez-assistant/protocol";
import { conversationItemId, RuntimeProjectionStore } from "../../src/stores/RuntimeProjectionStore";
import { childKey } from "../../src/stores/liveExecutionProjection";

const application: ApplicationSnapshot = {
  runtime_lifecycle: "running",
  ...sidebarFixture({ active_next: null, archived_next: null, active: [], archived: [] }),

  configuration: {
    config_path: null,
    revision: "fixture-revision",
    state: "ready",
    schema_version: 1,
    issues: [],
  },
  providers: [], model_settings: { default_model: null, vision_model: null },
  workspaces: [],


  controller_availability: { status: "unavailable" },
  additional_controller_count: 0,
  capabilities: {
    conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true, user_message_edit: false,
    tool_detail: true,
    queue_control: true,
    approval_queue: true,
    child_task_view: true,
    conversation_search: true,
  },
};

describe("RuntimeProjectionStore", () => {
  it("isolates group pages and discards session and project pages after a snapshot refresh", () => {
    const store = new RuntimeProjectionStore();
    const session = sessionView(page(1, [], null, false)).session;
    const groups = ["a", "b"].map(workspace_id => ({ group: { type: "workspace" as const, workspace_id }, filter: "active" as const,
      sessions: [{ ...session, session_id: workspace_id, workspace_id }], next_offset: 1 }));
    store.applyApplicationSnapshot({ observed_sequence: 2, value: { ...application, session_groups: groups, workspaces_next_offset: 50 } });
    const revision = store.list_revision;
    for (const group of groups) store.appendSessionGroup({ ...group, sessions: [{ ...session, session_id: `${group.group.workspace_id}-more` }], next_offset: null }, 1, revision);
    expect(store.sessionGroup("a", "active")?.sessions.map(row => row.session_id)).toEqual(["a", "a-more"]);
    expect(store.sessionGroup("b", "active")?.sessions.map(row => row.session_id)).toEqual(["b", "b-more"]);
    store.refreshApplicationSnapshot({ observed_sequence: 3, value: { ...application, session_groups: groups, workspaces_next_offset: 50 } });
    store.appendSessionGroup({ ...groups[0], sessions: [{ ...session, session_id: "stale" }], next_offset: null }, 1, revision);
    store.appendWorkspacePage([{ workspace_id: "stale", label: "stale", lifecycle: "active", user_directory: null, agent_directory: "/fixture", additional_directories: [], created_at_ms: 0, updated_at_ms: 0, removed_at_ms: null }], 50, null, revision);
    expect(store.sessionGroup("a", "active")?.sessions.map(row => row.session_id)).toEqual(["a"]);
    expect(store.application?.workspaces).toEqual([]);
    expect(store.application?.workspaces_next_offset).toBe(50);
    const current = store.list_revision;
    store.refreshApplicationSnapshot({ observed_sequence: 1, value: application });
    expect(store.list_revision).toBe(current);
  });

  it("resolves off-page selection from the view without adding a sidebar row", () => {
    const store = new RuntimeProjectionStore();
    const view = sessionView(page(1, [], null, false));
    store.applyApplicationSnapshot({ observed_sequence: 1, value: { ...application, pinned_sessions: [{ ...view.session, title: "old pin" }] } });
    store.applySessionSnapshot({ observed_sequence: 2, value: view });
    expect(store.sessionSummary(view.session.session_id)).toEqual(view.session);
    expect(store.loadedSessions()).toEqual([]);
    store.refreshApplicationSnapshot({ observed_sequence: 3, value: application });
    expect(store.sessionSummary(view.session.session_id)).toEqual(view.session);
  });

  it("isolates identical child IDs, watermarks and pagination by the complete owner", () => {
    const store = new RuntimeProjectionStore();
    const view = (session_id: string, generation: number): ChildTaskViewSnapshot => ({
      task: {
        task: {
          session_id, child_task_id: "shared-child", parent_run_id: "shared-run",
          parent_tool_call_id: "shared-call", title: session_id, status: "completed",
          variant: "build", cancel_requested: false, final_text: session_id, error: null,
          created_at_ms: 1, started_at_ms: 2, finished_at_ms: 3,
          inherited: true, body_unavailable: false,
        },
        usage: { accumulated: null }, pending_approval_count: 0, can_cancel: false,
      },
      approval_ids: [],
      conversation: { owner: { type: "child_task", session_id, child_task_id: "shared-child" },
        generation, items: [assistant("shared-message")], previous_cursor: "previous", has_more: true },
    });
    expect(store.applyChildTaskSnapshot({ observed_sequence: 100, value: view("source", 1) })).toBe(true);
    expect(store.applyChildTaskSnapshot({ observed_sequence: 2, value: view("branch", 1) })).toBe(true);
    expect(store.child_task_views.get(childKey("source", "shared-child"))?.task.task.title).toBe("source");
    expect(store.child_task_views.get(childKey("branch", "shared-child"))?.task.task.title).toBe("branch");
    expect(store.beginLoadingPreviousChild("source", "shared-child")).toBe(true);
    expect(store.beginLoadingPreviousChild("branch", "shared-child")).toBe(true);
    store.failLoadingPreviousChild("source", "shared-child", "source failure");
    expect(store.child_conversation_histories.get(childKey("branch", "shared-child"))?.is_loading_previous).toBe(true);
    expect(store.applyPreviousChildConversationPage("branch", "shared-child", {
      observed_sequence: 101, value: view("source", 1).conversation,
    })).toBe(false);
    expect(store.applyChildTaskSnapshot({ observed_sequence: 3, value: view("branch", 2) })).toBe(true);
    expect(store.child_conversation_histories.get(childKey("source", "shared-child"))?.generation).toBe(1);
    expect(childKey("a:b", "c")).not.toBe(childKey("a", "b:c"));
  });

  it("preserves a single pending previous request across a streaming tail refresh", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({ observed_sequence: 4, value: sessionView(page(1, [assistant("m3"), assistant("m4")], "c2", true)) });
    expect(store.beginLoadingPrevious("session-1")).toBe(true);
    const pending = store.conversation_histories.get("session-1")?.previous_request;
    store.applySessionSnapshot({ observed_sequence: 10, value: sessionView(page(1, [assistant("m4"), assistant("m5")], "c3", true)) });
    expect(store.conversation_histories.get("session-1")?.previous_request).toBe(pending);
    expect(store.beginLoadingPrevious("session-1")).toBe(false);
    expect(store.applyPreviousConversationPage("session-1", { observed_sequence: 5, value: page(1, [assistant("m1"), assistant("m2")], null, false) })).toBe(true);
    expect(store.conversation_histories.get("session-1")?.items.map(conversationItemId)).toEqual(["m1", "m2", "m3", "m4", "m5"]);
    expect(store.observed_sequence).toBe(0);
    expect(store.applySessionSnapshot({ observed_sequence: 9, value: sessionView(page(1, [], null, false)) })).toBe(false);
  });

  it("shows a committed receipt until its history page arrives and ignores replay after a newer run", () => {
    const store = new RuntimeProjectionStore();
    const run: RunSnapshot = { run_id: "run", session_id: "session-1", input_id: "input", attempt: 1,
      status: "running", created_at_ms: 2, finished_at_ms: null, variant: "build", approval_mode: "ask",
      cancel_requested: false, reasoning_parts: [], tools: [], text: "", error: null };
    const message = { message_id: "new-user", input_id: "input", text: "介入", created_at_ms: 1,
      inherited: false, attachment_ids: [], quotes: [], source: { type: "user" as const } };
    const before = sessionView(page(1, [assistant("old-tail")], "prev", true));
    const receipt = { input_id: "input", run, user_message: message, generation: 1 };
    store.applySessionSnapshot({ observed_sequence: 2, value: before });
    expect(store.applyInputReceipt("session-1", receipt)).toBe(true);
    expect(store.applyInputReceipt("session-1", receipt)).toBe(true);
    expect(store.input_receipts.get("session-1")).toHaveLength(1);
    store.applySessionSnapshot({ observed_sequence: 3, value: before });
    expect(store.input_receipts.get("session-1")).toHaveLength(1);
    expect(store.conversation_histories.get("session-1")?.previous_cursor).toBe("prev");
    store.applySessionSnapshot({ observed_sequence: 4, value: sessionView(page(1,
      [assistant("old-tail"), assistant("old-final"), { ...message, type: "user" }], "prev", true)) });
    expect(store.input_receipts.has("session-1")).toBe(false);
    expect(store.conversation_histories.get("session-1")?.items.map(conversationItemId)).toEqual(["old-tail", "old-final", "new-user"]);
    store.applySessionSnapshot({ observed_sequence: 6, value: { ...sessionView(page(1, [assistant("newer-tail")], "prev", true)),
      runs: [{ ...run, run_id: "newer-run", input_id: "newer-input", created_at_ms: 5 }] } });
    expect(store.applyInputReceipt("session-1", receipt)).toBe(false);
    expect(store.input_receipts.has("session-1")).toBe(false);
  });

  it("uses a snapshot watermark and detects an event gap", () => {
    const store = new RuntimeProjectionStore();
    store.applyApplicationSnapshot({ observed_sequence: 4, value: application });

    expect(store.acceptEvent(envelope(4))).toBe("ignored");
    expect(store.acceptEvent(envelope(5))).toBe("accepted");
    expect(store.acceptEvent(envelope(7))).toBe("gap");
    expect(store.is_stale).toBe(true);
    expect(store.observed_sequence).toBe(5);
  });

  it("does not let a projection refresh consume unseen stream events", () => {
    const store = new RuntimeProjectionStore();
    store.applyApplicationSnapshot({ observed_sequence: 4, value: application });
    store.refreshApplicationSnapshot({ observed_sequence: 6, value: application });

    expect(store.observed_sequence).toBe(4);
    expect(store.acceptEvent(envelope(5))).toBe("accepted");
    expect(store.acceptEvent(envelope(6))).toBe("accepted");
  });

  it("clears projections when the Runtime instance changes", () => {
    const store = new RuntimeProjectionStore();
    store.applyApplicationSnapshot({ observed_sequence: 8, value: application });
    store.resetForInstance();

    expect(store.application).toBeNull();
    expect(store.observed_sequence).toBe(0);
    expect(store.is_stale).toBe(true);
  });

  it("prepends older pages without duplicating stable message IDs", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({
      observed_sequence: 8,
      value: sessionView(page(3, [assistant("message-3"), assistant("message-4")], "cursor-2", true)),
    });

    expect(store.beginLoadingPrevious("session-1")).toBe(true);
    expect(store.applyPreviousConversationPage("session-1", {
      observed_sequence: 9,
      value: page(3, [assistant("message-1"), assistant("message-2"), assistant("message-3")], null, false),
    })).toBe(true);

    const history = store.conversation_histories.get("session-1");
    expect(history?.items.map(conversationItemId)).toEqual([
      "message-1",
      "message-2",
      "message-3",
      "message-4",
    ]);
    expect(history?.has_more).toBe(false);
    expect(history?.is_loading_previous).toBe(false);
  });

  it("replaces history when a latest page belongs to a new generation", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({
      observed_sequence: 4,
      value: sessionView(page(1, [assistant("old-message")], null, false)),
    });
    store.applySessionSnapshot({
      observed_sequence: 5,
      value: sessionView(page(2, [assistant("new-message")], null, false)),
    });

    const history = store.conversation_histories.get("session-1");
    expect(history?.generation).toBe(2);
    expect(history?.items.map(conversationItemId)).toEqual(["new-message"]);
  });

  it("rejects a session snapshot whose conversation belongs to another session", () => {
    const store = new RuntimeProjectionStore();
    const mismatched = sessionView({
      ...page(1, [assistant("foreign-message")], null, false),
      owner: { type: "main_session", session_id: "session-2" },
    });

    expect(store.applySessionSnapshot({ observed_sequence: 4, value: mismatched })).toBe(false);
    expect(store.session_views.has("session-1")).toBe(false);
    expect(store.conversation_histories.has("session-1")).toBe(false);
  });

  it("ignores an older session snapshot that finishes after a newer snapshot", () => {
    const store = new RuntimeProjectionStore();
    expect(store.applySessionSnapshot({
      observed_sequence: 8,
      value: sessionView(page(1, [assistant("newer-message")], null, false)),
    })).toBe(true);

    expect(store.applySessionSnapshot({
      observed_sequence: 7,
      value: sessionView(page(1, [assistant("late-old-message")], null, false)),
    })).toBe(false);
    expect(store.conversation_histories.get("session-1")?.items.map(conversationItemId)).toEqual([
      "newer-message",
    ]);
  });

  it("replaces a same-generation latest page when it has no continuous cached boundary", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({
      observed_sequence: 4,
      value: sessionView(page(1, [assistant("unrelated-cached-message")], null, false)),
    });

    store.applySessionSnapshot({
      observed_sequence: 5,
      value: sessionView(page(1, [assistant("authoritative-message")], "server-cursor", true)),
    });

    const history = store.conversation_histories.get("session-1");
    expect(history?.items.map(conversationItemId)).toEqual(["authoritative-message"]);
    expect(history?.previous_cursor).toBe("server-cursor");
    expect(history?.has_more).toBe(true);
  });

  it("retains loaded previous pages when a same-generation latest page overlaps", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({
      observed_sequence: 4,
      value: sessionView(page(1, [assistant("message-3"), assistant("message-4")], "cursor-2", true)),
    });
    expect(store.applyPreviousConversationPage("session-1", {
      observed_sequence: 5,
      value: page(1, [assistant("message-1"), assistant("message-2"), assistant("message-3")], null, false),
    })).toBe(true);

    store.applySessionSnapshot({
      observed_sequence: 6,
      value: sessionView(page(1, [assistant("message-3"), assistant("message-4"), assistant("message-5")], "cursor-2", true)),
    });

    expect(store.conversation_histories.get("session-1")?.items.map(conversationItemId)).toEqual([
      "message-1",
      "message-2",
      "message-3",
      "message-4",
      "message-5",
    ]);
  });

  it("uses the server page as authority after a compacted generation change", () => {
    const store = new RuntimeProjectionStore();
    store.applySessionSnapshot({
      observed_sequence: 4,
      value: sessionView(page(1, [assistant("old-memory-only-message")], null, false)),
    });
    const compacted = page(
      2,
      [contextSummary("summary-1"), assistant("retained-message")],
      "server-history-cursor",
      true,
    );

    store.applySessionSnapshot({
      observed_sequence: 5,
      value: { session: { session_id: "session-1" }, conversation: compacted } as unknown as SessionViewSnapshot,
    });

    const history = store.conversation_histories.get("session-1");
    expect(history?.items.map(conversationItemId)).toEqual(["summary-1", "retained-message"]);
    expect(history?.previous_cursor).toBe("server-history-cursor");
    expect(history?.has_more).toBe(true);
  });

});

function envelope(sequence: number): RuntimeEventEnvelope {
  return {
    sequence,
    emitted_at_ms: 1,
    event: { type: "config_changed" },
  };
}

function page(
  generation: number,
  items: ConversationItem[],
  previous_cursor: string | null,
  has_more: boolean,
): ConversationPage {
  return {
    owner: { type: "main_session", session_id: "session-1" },
    generation,
    items,
    previous_cursor,
    has_more,
  };
}

function sessionView(conversation: ConversationPage): SessionViewSnapshot {
  return {
    session: { session_id: "session-1" },
    conversation,
  } as unknown as SessionViewSnapshot;
}

function assistant(message_id: string): ConversationItem {
  return {
    type: "assistant",
    inherited: false,
    partial: false,
    message_id,
    run_id: null,
    attempt: null,
    created_at_ms: null,
    finished_at_ms: null,
    status: "completed",
    segments: [],
    usage: null,
    can_fork: false,
    fork_point: null,
    feedback: null,
  };
}

function contextSummary(message_id: string): ConversationItem {
  return {
    type: "context_summary",
    message_id,
    text: "summary",
  };
}
