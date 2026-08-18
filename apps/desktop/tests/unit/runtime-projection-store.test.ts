import { describe, expect, it } from "vitest";
import type {
  ApplicationSnapshot,
  ConversationItem,
  ConversationPage,
  RuntimeEventEnvelope,
} from "../../src/generated/assistant-protocol";
import { conversationItemId, RuntimeProjectionStore } from "../../src/stores/RuntimeProjectionStore";

const application: ApplicationSnapshot = {
  runtime_lifecycle: "running",
  configuration: {
    config_path: null,
    revision: "fixture-revision",
    state: "ready",
    schema_version: 1,
    default_model: "local",
    issues: [],
  },
  models: [],
  workspaces: [],
  active_sessions: [],
  archived_sessions: [],
  capabilities: {
    conversation_paging: true,
    tool_detail: true,
    queue_control: true,
    approval_queue: true,
    child_task_view: true,
    conversation_search: true,
  },
};

describe("RuntimeProjectionStore", () => {
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
    store.applyLocatedConversationPage("session-1", {
      observed_sequence: 8,
      value: page(3, [assistant("message-3"), assistant("message-4")], "cursor-2", true),
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

  it("replaces history when a located page belongs to a new generation", () => {
    const store = new RuntimeProjectionStore();
    store.applyLocatedConversationPage("session-1", {
      observed_sequence: 4,
      value: page(1, [assistant("old-message")], null, false),
    });
    store.applyLocatedConversationPage("session-1", {
      observed_sequence: 5,
      value: page(2, [assistant("new-message")], null, false),
    });

    const history = store.conversation_histories.get("session-1");
    expect(history?.generation).toBe(2);
    expect(history?.items.map(conversationItemId)).toEqual(["new-message"]);
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

function assistant(message_id: string): ConversationItem {
  return {
    type: "assistant",
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
