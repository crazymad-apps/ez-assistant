import { sidebarFixture } from "../support/sidebarFixture";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { ConnectionStore } from "../../src/stores/ConnectionStore";
import { NavigationStore } from "../../src/stores/NavigationStore";
import { RuntimeProjectionStore } from "../../src/stores/RuntimeProjectionStore";
import { LiveExecutionStore } from "../../src/stores/LiveExecutionStore";
import type { ApplicationSnapshot, ConversationPage, RunSnapshot, SessionSummary, SessionViewSnapshot } from "@ez-assistant/protocol";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";
import { RuntimeClientError } from "../../src/runtime-client/RuntimeClient";

const transport = vi.hoisted(() => ({ command: vi.fn(), events: vi.fn(), ready: vi.fn(), bootstrap: vi.fn(), refresh: vi.fn() }));
vi.mock("../../src/native-bridge/runtimeBootstrap", () => ({ bootstrapRuntime: transport.bootstrap }));
vi.mock("../../src/native-bridge/runtimeConnection", () => ({ refreshRuntimeConnection: transport.refresh }));
vi.mock("../../src/runtime-client/RuntimeClient", () => ({
  RuntimeClientError: class extends Error {
    readonly code: string;
    constructor(code: string, message: string) { super(message); this.code = code; }
  },
  RuntimeClient: class {
    instance_id: string; address: string; capabilities: RuntimeBootstrap["capabilities"];
    constructor(bootstrap: RuntimeBootstrap) { this.instance_id = bootstrap.instance_id; this.address = bootstrap.base_url; this.capabilities = bootstrap.capabilities; }
    command(command: unknown, options?: unknown) { return transport.command(this.address, command, options); }
    connectEvents(callbacks: unknown) { return Promise.resolve(transport.events(this.address, callbacks) ?? { closed: new Promise<void>(() => undefined) }); }
    waitUntilReady(receive: unknown) { return transport.ready(receive); }
    dispose() {}
  },
}));
const application: ApplicationSnapshot = {
  runtime_lifecycle: "running",
  ...sidebarFixture({ active_next: null, archived_next: null, active: [], archived: [] }),
   configuration: { config_path: null, revision: "fixture", state: "ready", schema_version: 1, issues: [] }, providers: [], model_settings: { default_model: null, vision_model: null }, workspaces: [],   controller_availability: { status: "unavailable" }, additional_controller_count: 0,
  capabilities: { conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true, user_message_edit: false, tool_detail: true, queue_control: true, approval_queue: true, child_task_view: true, conversation_search: true },
};
const bootstrap = (origin: string): RuntimeBootstrap => ({ base_url: origin, instance_id: origin, binding_id: origin, target_kind: "remote", access_token: "fixture", capabilities: { mode: "personal", min_compatible_version: "0.25.2", runtime_version: "0.25.2", max_command_bytes: 1048576, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["web_login", "startup_diagnostics"] }, started_runtime: false });
const result = (revision: string) => ({ type: "get_application_snapshot", payload: { snapshot: { observed_sequence: 0, value: { ...structuredClone(application), configuration: { ...application.configuration, revision } } } } });
const owners: RuntimeLifecycleCoordinator[] = [];
function create() {
  const connection = new ConnectionStore(), projection = new RuntimeProjectionStore(), navigation = new NavigationStore();
  const live_execution = new LiveExecutionStore();
  const report_error = vi.fn();
  const coordinator = new RuntimeLifecycleCoordinator({ connection, projection, navigation, live_execution, report_interaction_error: report_error, refresh_device_gateway: vi.fn(), mark_device_gateway_stale: vi.fn(), on_title_generation_finished: vi.fn() });
  owners.push(coordinator); return { coordinator, connection, projection, navigation, live_execution, report_error };
}
beforeEach(() => { vi.clearAllMocks(); transport.command.mockImplementation(async (address: string) => result(address)); transport.refresh.mockImplementation(async (value: RuntimeBootstrap) => value); });
afterEach(() => { owners.splice(0).forEach((owner) => owner.dispose()); vi.restoreAllMocks(); vi.useRealTimers(); });

function sessionResult(id: string, title = id, active_run: RunSnapshot | null = null, observed_sequence = 0) {
  return { type: "get_session_view", payload: { snapshot: { observed_sequence,
    value: { session: { session_id: id, title }, active_run, runs: active_run ? [active_run] : [], child_tasks: [],
      conversation: { owner: { type: "main_session", session_id: id }, generation: 1, items: [], previous_cursor: null, has_more: false } },
  } } };
}

const activeRun = (): RunSnapshot => ({ session_id: "s", run_id: "r", input_id: "i", attempt: 1,
  created_at_ms: 1, started_at_ms: 2, finished_at_ms: null, status: "running", variant: "build", approval_mode: "ask",
  cancel_requested: false, reasoning_parts: [], text: "base", tools: [], error: null, active_step: 1,
});

const summary = (session_id: string): SessionSummary => ({
  session_id, title: session_id, model_selection: null, lifecycle: "active", role: "standard",
  current_variant: "build", approval_mode: "ask", workspace_id: null, active_run_id: null,
  message_count: 0, queued_input_count: 0, resume_required: false, created_at_ms: 1, updated_at_ms: 1,
  archived_at_ms: null, is_pinned: false, title_origin: "user", pending_approval_count: 0,
  active_child_count: 0, active_run_status: null,
});

function seedGroupPage(projection: RuntimeProjectionStore, filter: "active" | "archived", sessions: SessionSummary[], next_offset: number | null) {
  projection.appendSessionGroup({ group: { type: "unbound" }, filter, sessions, next_offset }, 0, projection.list_revision);
}

describe("bounded owner snapshot reads", () => {
  it("omits collapsed groups while retaining their loaded prefix for reopening", async () => {
    const { coordinator, projection, navigation } = create();
    await coordinator.connect(bootstrap("http://a"));
    seedGroupPage(projection, "active", Array.from({ length: 40 }, (_, i) => summary(`s-${i}`)), 40);
    navigation.setSectionOpen("unbound", false);
    transport.command.mockClear();
    transport.command.mockImplementation(async (address, command) => {
      expect(command.payload.session_groups).toEqual([]);
      const response = result(address);
      response.payload.snapshot.value.session_groups = [];
      return response;
    });
    await coordinator.loadApplication();
    expect(projection.application?.session_groups).toEqual([]);
    navigation.setSectionOpen("unbound", true);
    transport.command.mockImplementation(async (address, command) => {
      expect(command.payload.session_groups).toEqual([{ group: { type: "unbound" }, filter: "active", limit: 40 }]);
      return result(address);
    });
    await coordinator.loadApplication();
    expect(transport.command).toHaveBeenCalledTimes(2);
  });

  it("refreshes the loaded list prefixes in one snapshot with authoritative ordering and offsets", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    const loaded = Array.from({ length: 500 }, (_, index) => summary(`old-${index}`));
    seedGroupPage(projection, "active", loaded, 500);
    seedGroupPage(projection, "archived", Array.from({ length: 235 }, (_, index) => ({ ...summary(`archived-${index}`), lifecycle: "archived" as const })), null);
    transport.command.mockClear();
    transport.command.mockImplementation(async (address, command) => {
      expect(command).toEqual({ type: "get_application_snapshot", payload: { workspaces_limit: 10, session_groups: [{ group: { type: "unbound" }, filter: "active", limit: 500 }] } });
      const response = result(address);
      response.payload.snapshot.value.session_groups[0].sessions = [summary("new-pinned"), ...loaded.slice(0, 499)];
      response.payload.snapshot.value.session_groups[0].next_offset = 500;
      return response;
    });
    await coordinator.loadApplication();
    expect(transport.command).toHaveBeenCalledOnce();
    expect(projection.application?.session_groups[0].sessions.map((session) => session.session_id)).toEqual(["new-pinned", ...loaded.slice(0, 499).map((session) => session.session_id)]);
    expect(projection.application?.session_groups[0].next_offset).toBe(500);
    expect(projection.application?.session_groups[1].sessions).toEqual([]);
    expect(projection.application?.session_groups[1].next_offset).toBeNull();
  });

  it("retries the same enlarged snapshot at most three times without fetching individual pages", async () => {
    const { coordinator, projection, report_error } = create();
    await coordinator.connect(bootstrap("http://a"));
    seedGroupPage(projection, "active", [summary("previous")], 500);
    const previous = projection.application;
    vi.useFakeTimers();
    transport.command.mockClear();
    transport.command.mockRejectedValue(new RuntimeClientError("snapshot_busy", "busy"));
    const loading = coordinator.loadApplication();
    await vi.advanceTimersByTimeAsync(1_000);
    await loading;
    expect(transport.command).toHaveBeenCalledTimes(3);
    for (const [, command] of transport.command.mock.calls) {
      expect(command).toEqual({ type: "get_application_snapshot", payload: { workspaces_limit: 10, session_groups: [{ group: { type: "unbound" }, filter: "active", limit: 500 }] } });
    }
    expect(report_error).toHaveBeenCalledWith("会话暂时繁忙，请重试");
    expect(projection.application).toBe(previous);
  });

  it("keeps an off-page selection without fetching or injecting its summary", async () => {
    const { coordinator, projection, navigation, connection, report_error } = create();
    await coordinator.connect(bootstrap("http://a"));
    const previous = projection.application;
    navigation.selectSession("older", false);
    vi.useFakeTimers();
    transport.command.mockClear();
    transport.command.mockImplementation(async (address, command) => {
      if (command.type === "get_session") throw new RuntimeClientError("snapshot_busy", "busy");
      return result(address);
    });
    const loading = coordinator.loadApplication();
    await vi.advanceTimersByTimeAsync(1_000);
    await loading;
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_application_snapshot")).toHaveLength(1);
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session")).toHaveLength(0);
    expect(report_error).not.toHaveBeenCalled();
    expect(projection.application).not.toBe(previous);
    expect(navigation.selected_session_id).toBe("older");
    expect(connection.state).toBe("connected");
  });

  it("enforces the application deadline while keeping an off-page selection", async () => {
    const { coordinator, projection, navigation, report_error } = create();
    await coordinator.connect(bootstrap("http://a"));
    seedGroupPage(projection, "active", [summary("previous")], 500);
    navigation.selectSession("older", false);
    const previous = projection.application;
    vi.useFakeTimers();
    transport.command.mockClear();
    transport.command.mockImplementation((address, command) => {
      if (command.type === "get_application_snapshot") {
        expect(command.payload.session_groups[0].limit).toBe(500);
        return new Promise((resolve) => window.setTimeout(() => resolve(result(address)), 16_000));
      }
      expect(command.type).toBe("get_session");
      return new Promise(() => {});
    });
    const loading = coordinator.loadApplication();
    await vi.advanceTimersByTimeAsync(14_999);
    expect(report_error).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    await loading;
    expect(report_error).toHaveBeenCalledWith("读取会话超时，请重试");
    expect(transport.command).toHaveBeenCalledTimes(1);
    const signals = transport.command.mock.calls.map(([, , options]) => options.signal as AbortSignal);
    expect(signals.every((signal) => signal === signals[0] && signal.aborted)).toBe(true);
    expect(projection.application).toBe(previous);
  });

  it("preserves loaded counts on same-instance reconnect and resets them for a different instance", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    seedGroupPage(projection, "active", [summary("previous")], 500);
    transport.command.mockClear();
    await coordinator.connect(bootstrap("http://a"));
    expect(transport.command).toHaveBeenCalledOnce();
    expect(transport.command.mock.calls[0]![1]).toEqual({ type: "get_application_snapshot", payload: { workspaces_limit: 10, session_groups: [{ group: { type: "unbound" }, filter: "active", limit: 500 }] } });
    seedGroupPage(projection, "active", [summary("previous")], 500);
    transport.command.mockClear();
    await coordinator.connect(bootstrap("http://b"));
    expect(transport.command).toHaveBeenCalledOnce();
    expect(transport.command.mock.calls[0]![1]).toEqual({ type: "get_application_snapshot", payload: {} });
  });

  it("preserves a gap rebase requested during an expanded application refresh and resumes the global stream", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    seedGroupPage(projection, "active", [summary("previous")], 500);
    transport.command.mockClear();
    let finish!: (value: ReturnType<typeof result>) => void;
    const fresh = result("fresh"); fresh.payload.snapshot.observed_sequence = 2;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; })).mockResolvedValue(fresh);
    const loading = coordinator.loadApplication();
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onGap();
    callbacks.onEvent({ sequence: 2, emitted_at_ms: 2, event: { type: "config_changed", revision: "fresh" } });
    finish(result("discard"));
    await loading;
    expect(projection.observed_sequence).toBe(2);
    expect(projection.application?.configuration.revision).toBe("fresh");
    callbacks.onEvent({ sequence: 3, emitted_at_ms: 3, event: { type: "runtime_shutting_down" } });
    expect(projection.observed_sequence).toBe(3);
    expect(transport.command).toHaveBeenCalledTimes(2);
    for (const [, command] of transport.command.mock.calls) {
      expect(command).toEqual({ type: "get_application_snapshot", payload: { workspaces_limit: 10, session_groups: [{ group: { type: "unbound" }, filter: "active", limit: 500 }] } });
    }
  });

  it("deduplicates SSE already included in one owner snapshot while retaining another owner's events", async () => {
    const { coordinator, navigation, projection, live_execution } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockResolvedValue(sessionResult("s", "s", activeRun(), 2));
    await coordinator.loadSession("s");
    expect(projection.observed_sequence).toBe(0);
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 1, emitted_at_ms: 1, event: { type: "text_delta", session_id: "s", run_id: "r", step: 1, part_id: "p", delta: "base" } });
    callbacks.onEvent({ sequence: 2, emitted_at_ms: 2, event: { type: "run_started", session_id: "s", run_id: "r" } });
    callbacks.onEvent({ sequence: 3, emitted_at_ms: 3, event: { type: "text_delta", session_id: "other", run_id: "r", step: 1, part_id: "p", delta: "other" } });
    live_execution.flush();
    const text = (id: string) => live_execution.runForSession(id)?.steps.flatMap((step) => step.segments.flatMap((part) => part.type === "text" ? [part.text] : [])).join("");
    expect(text("s")).toBe("base");
    expect(text("other")).toBe("other");
    expect(projection.observed_sequence).toBe(3);
  });

  it("bridges a disjoint same-generation latest page to the retained reading history", async () => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    const view = sessionResult("s").payload.snapshot.value as unknown as SessionViewSnapshot;
    const conversation = (ids: string[], cursor: string | null): ConversationPage => ({
      ...view.conversation, previous_cursor: cursor, has_more: cursor !== null,
      items: ids.map(message_id => ({
        type: "user", message_id, text: message_id, inherited: false, input_id: null, attachment_ids: [],
        quotes: [], source: { type: "user" }, created_at_ms: null,
      })),
    });
    projection.applySessionSnapshot({ observed_sequence: 1, value: { ...view, conversation: conversation(["m1", "m2"], null) } });
    transport.command.mockClear()
      .mockResolvedValueOnce({ payload: { snapshot: { observed_sequence: 5, value: { ...view, conversation: conversation(["m5", "m6"], "newer") } } } })
      .mockResolvedValueOnce({ payload: { snapshot: { observed_sequence: 6, value: conversation(["m3", "m4"], "older") } } })
      .mockResolvedValueOnce({ payload: { snapshot: { observed_sequence: 7, value: conversation(["m1", "m2"], null) } } });
    await coordinator.loadSession("s");
    expect(transport.command.mock.calls.slice(1).map(([, command]) => command.payload.cursor)).toEqual(["newer", "older"]);
    expect(projection.conversation_histories.get("s")?.items.map(item => item.message_id)).toEqual(["m1", "m2", "m3", "m4", "m5", "m6"]);
    expect(projection.observed_sequence).toBe(0);
    expect(projection.sessionSnapshotSequence("s")).toBe(5);
  });

  it("resynchronizes an overflowing initial stream without installing the truncated baseline", async () => {
    const { coordinator, projection, connection } = create();
    let finish!: (value: ReturnType<typeof result>) => void;
    const fresh = result("fresh"); fresh.payload.snapshot.observed_sequence = 1;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; })).mockResolvedValue(fresh);
    const connecting = coordinator.connect(bootstrap("http://a"));
    await vi.waitFor(() => expect(transport.command).toHaveBeenCalledOnce());
    transport.events.mock.calls.at(-1)![1].onEvent({ sequence: 1, emitted_at_ms: 1,
      event: { type: "text_delta", session_id: "other", run_id: "r", step: 1, part_id: "p", delta: "x".repeat(2 * 1024 * 1024) } });
    finish(result("discard"));
    await connecting;
    expect(projection.application?.configuration.revision).toBe("fresh");
    expect(projection.observed_sequence).toBe(1);
    expect(connection.state).toBe("connected");
    expect(transport.command).toHaveBeenCalledTimes(2);
  });

  it("does not turn a child's streaming text into repeated detail and application reads", async () => {
    vi.useFakeTimers();
    const { coordinator, navigation } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockResolvedValue(sessionResult("s"));
    await coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    for (let index = 1; index <= 3; index++) {
      callbacks.onEvent({ sequence: index, emitted_at_ms: index, event: { type: "child_task_event", session_id: "s", parent_run_id: "r", child_task_id: "child",
        event: { type: "text_delta", step: 1, part_id: "p", delta: "tail" } } });
      await vi.advanceTimersByTimeAsync(100);
    }
    expect(transport.command).toHaveBeenCalledTimes(2);
  });

  it.each(["count", "bytes"])("discards a snapshot on %s buffer overflow and resynchronizes within the same flow", async (limit) => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    let finish!: (value: ReturnType<typeof sessionResult>) => void;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }))
      .mockResolvedValue(sessionResult("s", "fresh", activeRun(), limit === "count" ? 2049 : 1));
    const apply = vi.spyOn(projection, "applySessionSnapshot");
    const loading = coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    for (let index = 1; index <= (limit === "count" ? 2049 : 1); index++) {
      callbacks.onEvent({ sequence: index, emitted_at_ms: index, event: { type: "text_delta", session_id: "s", run_id: "r", step: 1, part_id: "p", delta: limit === "count" ? "x" : "x".repeat(2 * 1024 * 1024) } });
    }
    finish(sessionResult("s", "discard", activeRun()));
    await loading;
    expect(apply).toHaveBeenCalledOnce();
    expect(projection.session_views.get("s")?.session.title).toBe("fresh");
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(2);
  });

  it("does not include other-owner output in the buffer or move the global cursor with a session snapshot", async () => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    let finish!: (value: ReturnType<typeof sessionResult>) => void;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const loading = coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    for (let index = 1; index <= 2049; index++) callbacks.onEvent({ sequence: index, emitted_at_ms: index,
      event: { type: "text_delta", session_id: "other", run_id: "r", step: 1, part_id: "p", delta: "x" } });
    finish(sessionResult("s"));
    await loading;
    expect(projection.observed_sequence).toBe(2049);
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(1);
  });

  it("rejects a generation crossed by a reliable commit before installing the old snapshot", async () => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    let finish!: (value: ReturnType<typeof sessionResult>) => void;
    const fresh = sessionResult("s", "new generation", null, 1);
    fresh.payload.snapshot.value.conversation.generation = 2;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; })).mockResolvedValue(fresh);
    const apply = vi.spyOn(projection, "applySessionSnapshot");
    const loading = coordinator.loadSession("s");
    transport.events.mock.calls.at(-1)![1].onEvent({ sequence: 1, emitted_at_ms: 1,
      event: { type: "conversation_committed", owner: { type: "main_session", session_id: "s" }, generation: 2 } });
    finish(sessionResult("s", "old generation"));
    await loading;
    expect(apply).toHaveBeenCalledOnce();
    expect(projection.conversation_histories.get("s")?.generation).toBe(2);
  });

  it("installs the HTTP baseline and replays the later text once without losing its prefix", async () => {
    const { coordinator, navigation, live_execution } = create();
    const initial = result("a"); initial.payload.snapshot.observed_sequence = 10;
    transport.command.mockResolvedValueOnce(initial);
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    let finish!: (value: ReturnType<typeof sessionResult>) => void;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const loading = coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 11, emitted_at_ms: 20, event: { type: "text_delta", session_id: "s", run_id: "r", step: 1, part_id: "p", delta: " tail" } });
    live_execution.flush();
    finish(sessionResult("s", "s", activeRun(), 10));
    await loading;
    const texts = live_execution.runForSession("s")!.steps.flatMap((step) => step.segments.flatMap((part) => part.type === "text" ? [part.text] : []));
    expect(texts.join("")).toBe("base tail");
  });

  it("does not revert a later terminal event when an older running snapshot arrives", async () => {
    const { coordinator, navigation, live_execution } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    let finish!: (value: ReturnType<typeof sessionResult>) => void;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const loading = coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 1, emitted_at_ms: 10, event: { type: "run_started", session_id: "s", run_id: "r", started_at_ms: 2 } });
    callbacks.onEvent({ sequence: 2, emitted_at_ms: 20, event: { type: "run_finished", session_id: "s", run_id: "r", status: "failed", error: { code: "model_execution_failed", message: "later failure" }, finished_at_ms: 19 } });
    live_execution.flush();
    finish(sessionResult("s", "s", activeRun(), 1));
    await loading;
    expect(live_execution.runForSession("s")?.status).toBe("failed");
    expect(live_execution.runForSession("s")?.error_message).toBe("later failure");
  });

  it("recovers a transient busy response and releases the owner error state", async () => {
    vi.useFakeTimers();
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockRejectedValueOnce(new RuntimeClientError("snapshot_busy", "busy")).mockResolvedValue(sessionResult("s"));
    const loading = coordinator.loadSession("s");
    expect(projection.conversationLoadState("s").status).toBe("loading");
    await vi.advanceTimersByTimeAsync(250);
    await loading;
    expect(projection.conversationLoadState("s").status).toBe("idle");
    expect(projection.session_views.has("s")).toBe(true);
  });

  it("ends persistent busy after three requests without disconnecting the Host", async () => {
    vi.useFakeTimers();
    const { coordinator, connection, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockRejectedValue(new RuntimeClientError("snapshot_busy", "busy"));
    const loading = coordinator.loadSession("s");
    await vi.advanceTimersByTimeAsync(250);
    await vi.advanceTimersByTimeAsync(750);
    await loading;
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(3);
    expect(projection.conversationLoadState("s")).toEqual({ status: "error", error: "会话暂时繁忙，请重试" });
    expect(connection.state).toBe("connected");
  });

  it("ends an unresponsive body within the total budget and aborts the request", async () => {
    vi.useFakeTimers();
    const { coordinator, connection, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockReturnValue(new Promise(() => undefined));
    let ended = false;
    void coordinator.loadSession("s").then(() => { ended = true; });
    await vi.advanceTimersByTimeAsync(15_000);
    expect(ended).toBe(true);
    expect(transport.command.mock.calls.at(-1)![2]?.signal.aborted).toBe(true);
    expect(projection.conversationLoadState("s")).toEqual({ status: "error", error: "读取会话超时，请重试" });
    expect(connection.state).toBe("connected");
  });

  it("rejects the first A result after A-B-A and coalesces duplicate loads", async () => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    let resolve_old!: (value: ReturnType<typeof sessionResult>) => void;
    transport.command.mockReturnValueOnce(new Promise((resolve) => { resolve_old = resolve; }));
    navigation.selectSession("a", false);
    const old = coordinator.loadSession("a");
    void coordinator.loadSession("a");
    navigation.selectSession("b", false);
    transport.command.mockResolvedValue(sessionResult("b"));
    await coordinator.loadSession("b");
    navigation.selectSession("a", false);
    transport.command.mockResolvedValue(sessionResult("a", "new A"));
    await coordinator.loadSession("a");
    resolve_old(sessionResult("a", "old A"));
    await old;
    expect(projection.session_views.get("a")?.session.title).toBe("new A");
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(3);
  });

  it("keeps the second A loading when the cancelled A settles late", async () => {
    const { coordinator, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    const resolvers: Array<(value: ReturnType<typeof sessionResult>) => void> = [];
    transport.command.mockImplementation(() => new Promise((resolve) => resolvers.push(resolve)));
    navigation.selectSession("a", false);
    const first = coordinator.loadSession("a");
    const old_signal = transport.command.mock.calls.at(-1)![2].signal as AbortSignal;
    navigation.selectSession("b", false);
    navigation.selectSession("a", false);
    const second = coordinator.loadSession("a");
    expect(old_signal.aborted).toBe(true);
    resolvers[0]!(sessionResult("a", "old"));
    await first;
    expect(projection.conversationLoadState("a").status).toBe("loading");
    expect(projection.session_views.has("a")).toBe(false);
    resolvers[1]!(sessionResult("a", "new"));
    await second;
    expect(projection.conversationLoadState("a").status).toBe("idle");
    expect(projection.session_views.get("a")?.session.title).toBe("new");
  });

  it("includes backoff in one budget and does not retry an ordinary storage error", async () => {
    vi.useFakeTimers();
    const { coordinator, navigation, projection, connection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockImplementation(() => new Promise((_, reject) => {
      window.setTimeout(() => reject(new RuntimeClientError("snapshot_busy", "busy")), 14_900);
    }));
    const loading = coordinator.loadSession("s");
    await vi.advanceTimersByTimeAsync(15_000);
    await loading;
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(1);
    expect(projection.conversationLoadState("s").error).toBe("读取会话超时，请重试");
    transport.command.mockRejectedValue(new RuntimeClientError("storage_unavailable", "file unavailable"));
    await coordinator.loadSession("s");
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(2);
    expect(projection.conversationLoadState("s").error).toBe("file unavailable");
    expect(connection.state).toBe("connected");
  });

  it("bounds initial application loading and stops automatic business reconnects after busy", async () => {
    vi.useFakeTimers();
    const { coordinator, connection } = create();
    transport.command.mockRejectedValue(new RuntimeClientError("snapshot_busy", "busy"));
    const connecting = coordinator.connect(bootstrap("http://a"));
    await vi.advanceTimersByTimeAsync(1_000);
    await connecting;
    expect(transport.command).toHaveBeenCalledTimes(3);
    expect(connection.last_error_code).toBe("snapshot_busy");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(transport.command).toHaveBeenCalledTimes(3);
  });

  it("refreshes other-session summaries at the earliest timer without reloading the selection", async () => {
    vi.useFakeTimers();
    const { coordinator, navigation } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("s", false);
    transport.command.mockImplementation(async (address, command) => command.type === "get_session_view" ? sessionResult("s") : result(address));
    await coordinator.loadSession("s");
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 1, emitted_at_ms: 1, event: { type: "run_started", session_id: "other", run_id: "r" } });
    await vi.advanceTimersByTimeAsync(40);
    callbacks.onEvent({ sequence: 2, emitted_at_ms: 2, event: { type: "run_finished", session_id: "other", run_id: "r", status: "completed", error: null } });
    await vi.advanceTimersByTimeAsync(40);
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_application_snapshot")).toHaveLength(2);
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session_view")).toHaveLength(1);
  });
});

describe("selected Runtime lifecycle", () => {
  it("ignores native restart completion on the entry page until the user selects a connection", async () => {
    const { coordinator } = create();
    coordinator.prepareForNativeRuntimeMutation("restart");
    await coordinator.reconnectAfterNativeRuntimeMutation();
    expect(transport.bootstrap).not.toHaveBeenCalled();
    expect(transport.ready).not.toHaveBeenCalled();
    expect(transport.command).not.toHaveBeenCalled();
    await coordinator.reconnectAfterNativeRuntimeMutation(bootstrap("http://explicit"));
    expect(transport.ready).toHaveBeenCalled();
  });

  it("refreshes application capabilities when background MCP discovery completes", async () => {
    const { coordinator } = create();
    await coordinator.connect(bootstrap("http://a"));
    const before = transport.command.mock.calls.length;
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 1, emitted_at_ms: 1, event: { type: "mcp_registry_changed" } });
    await vi.waitFor(() => expect(transport.command.mock.calls.length).toBeGreaterThan(before));
    expect(transport.command.mock.calls.at(-1)![1].type).toBe("get_application_snapshot");
  });

  it("invalidates live tails synchronously before requesting gap recovery snapshots", async () => {
    const { coordinator, live_execution } = create();
    await coordinator.connect(bootstrap("http://a"));
    const invalidate = vi.spyOn(live_execution, "invalidateForGap");
    const callbacks = transport.events.mock.calls.at(-1)![1];

    callbacks.onGap();

    expect(invalidate).toHaveBeenCalledOnce();
  });

  it("retains an older selected session through reconnect without fetching every summary page", async () => {
    const { coordinator, projection, navigation } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("older", false);
    const older: SessionSummary = {
      session_id: "older", title: "Older", model_selection: { source: 'local' as const, provider_instance_id: "provider-1", model_id: "fixture" }, lifecycle: "active", role: "standard",
      current_variant: "build", approval_mode: "ask", workspace_id: null, active_run_id: null,
      message_count: 0, queued_input_count: 0, resume_required: false, created_at_ms: 1, updated_at_ms: 1,
      archived_at_ms: null, is_pinned: false, title_origin: "user", pending_approval_count: 0,
      active_child_count: 0, active_run_status: null,
    };
    transport.command.mockImplementation(async (address: string, command: { type: string }) => {
      if (command.type === "get_session") return { payload: { session: older } };
      const response = result(address);
      response.payload.snapshot.value.session_groups[0].next_offset = 100;
      return response;
    });
    // 避开本用例不关心的正文装配，仅验证连接后的摘要与选择恢复。
    vi.spyOn(coordinator, "loadSession").mockResolvedValue();
    coordinator.retryConnection();
    await vi.waitFor(() => expect(coordinator.loadSession).toHaveBeenCalledWith("older"));
    expect(projection.application?.session_groups[0].sessions).not.toContainEqual(older);
    expect(transport.command.mock.calls.some(([, command]) => command.type === "get_session")).toBe(false);
    expect(navigation.selected_session_id).toBe("older");
    expect(transport.command.mock.calls.some(([, command]) => command.type === "list_sessions")).toBe(false);
  });

  it("reconnects the selected remote binding without bootstrapping local", async () => {
    const { coordinator, connection } = create(); await coordinator.connect(bootstrap("http://b"));
    coordinator.retryConnection();
    await vi.waitFor(() => expect(transport.refresh).toHaveBeenCalledWith(bootstrap("http://b")));
    await vi.waitFor(() => expect(connection.state).toBe("connected"));
    expect(transport.bootstrap).not.toHaveBeenCalled(); expect(connection.address).toBe("http://b");
  });
  it("ignores an old application snapshot after A-B-A even when instance IDs match", async () => {
    const { coordinator, projection } = create(); await coordinator.connect(bootstrap("http://a"));
    let resolve!: (value: ReturnType<typeof result>) => void;
    transport.command.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    const old = coordinator.loadApplication();
    coordinator.prepareForNativeRuntimeMutation("restart"); await coordinator.connect(bootstrap("http://b"));
    coordinator.prepareForNativeRuntimeMutation("restart"); await coordinator.connect(bootstrap("http://a"));
    resolve(result("late-old-a")); await old;
    expect(projection.application?.configuration.revision).toBe("http://a");
  });
  it("does not apply initial snapshots after disposal", async () => {
    const { coordinator, projection } = create();
    let resolve!: (value: ReturnType<typeof result>) => void;
    transport.command.mockReturnValueOnce(new Promise((done) => { resolve = done; }));
    const connecting = coordinator.connect(bootstrap("http://a"));
    await vi.waitFor(() => expect(transport.command).toHaveBeenCalled());
    coordinator.dispose(); resolve(result("late-initial")); await connecting;
    expect(projection.application).toBeNull();
  });

  it("rejects a session response whose reliable conversation belongs to another session", async () => {
    const { coordinator, connection, navigation, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    navigation.selectSession("session-1", false);
    transport.command.mockImplementation(async (address: string, command: { type: string }) => {
      if (command.type === "get_session_view") {
        return {
          type: "get_session_view",
          payload: {
            snapshot: {
              observed_sequence: 1,
              value: {
                session: { session_id: "session-1" },
                conversation: {
                  owner: { type: "main_session", session_id: "session-2" },
                  generation: 1,
                  items: [],
                  previous_cursor: null,
                  has_more: false,
                },
              },
            },
          },
        };
      }
      return result(address);
    });

    await coordinator.loadSession("session-1");

    expect(connection.state).toBe("connected");
    expect(projection.conversationLoadState("session-1").status).toBe("error");
    expect(projection.conversation_histories.has("session-1")).toBe(false);
  });
});

it("waits for readiness before opening events or loading application data", async () => {
  const { coordinator, connection } = create();
  let ready!: () => void;
  transport.ready.mockImplementationOnce((receive) => {
    receive({ status: "starting", stage: "database_migration", error: null, database_version: null, target_version: "0.25.1" });
    return new Promise<void>((resolve) => { ready = resolve; });
  });
  const connecting = coordinator.connect(bootstrap("http://a"));
  await vi.waitFor(() => expect(connection.startup?.stage).toBe("database_migration"));
  expect(transport.events).not.toHaveBeenCalled();
  expect(transport.command).not.toHaveBeenCalled();
  ready(); await connecting;
  expect(connection.state).toBe("connected");
  expect(connection.startup).toBeNull();
});

it("ignores late readiness after the user closes a connection", async () => {
  const { coordinator, projection } = create();
  let ready!: () => void;
  transport.ready.mockImplementationOnce(() => new Promise<void>((resolve) => { ready = resolve; }));
  const connecting = coordinator.connect(bootstrap("http://a"));
  await vi.waitFor(() => expect(transport.ready).toHaveBeenCalled());
  coordinator.dispose(); ready(); await connecting;
  expect(transport.events).not.toHaveBeenCalled();
  expect(transport.command).not.toHaveBeenCalled();
  expect(projection.application).toBeNull();
});

it("preserves a required-update rejection and stops automatic business reconnects", async () => {
  vi.useFakeTimers();
  try {
    const { coordinator, connection } = create();
    transport.ready.mockRejectedValueOnce({ code: "center_protocol_incompatible", message: "需要更新后继续使用。" });
    await coordinator.connect(bootstrap("http://a"));
    expect(connection.last_error_code).toBe("center_protocol_incompatible");
    expect(transport.events).not.toHaveBeenCalled();
    expect(transport.command).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(10000);
    expect(transport.refresh).not.toHaveBeenCalled();
  } finally { vi.useRealTimers(); }
});


describe("network recovery beyond the initial retries", () => {
  it.each(["runtime_unavailable"])("keeps retrying %s and restores the same connection", async (code) => {
    vi.useFakeTimers();
    const { coordinator, connection, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    const previous = projection.application;
    transport.refresh.mockRejectedValue({ code, message: "offline" });
    coordinator.retryConnection();
    await vi.advanceTimersByTimeAsync(120_000);
    expect(transport.refresh.mock.calls.length).toBeGreaterThan(3);
    expect(transport.refresh.mock.calls.length).toBeLessThan(15);
    expect(projection.application).toBe(previous);
    expect(connection.state).toBe("disconnected");
    transport.refresh.mockImplementation(async (value: RuntimeBootstrap) => value);
    await vi.advanceTimersByTimeAsync(31_000);
    expect(connection.state).toBe("connected");
    expect(transport.bootstrap).not.toHaveBeenCalled();
    expect(transport.events).toHaveBeenCalledTimes(2);
    const calls = transport.refresh.mock.calls.length;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(transport.refresh).toHaveBeenCalledTimes(calls);
  });

  it("retries transport failure during readiness instead of treating it as failed initialization", async () => {
    vi.useFakeTimers();
    const { coordinator, connection } = create();
    transport.ready.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    await coordinator.connect(bootstrap("http://a"));
    expect(connection.last_error_code).toBe("runtime_unavailable");
    await vi.advanceTimersByTimeAsync(1_000);
    expect(connection.state).toBe("connected");
  });

  it.each(["authentication_required", "login_context_changed", "runtime_initialization_failed", "storage_unavailable", "runtime_unavailable"])("stops recovery on explicit %s", async (code) => {
    vi.useFakeTimers();
    const { coordinator } = create();
    transport.ready.mockRejectedValueOnce({ code, message: "rejected" });
    await coordinator.connect(bootstrap("http://a"));
    await vi.advanceTimersByTimeAsync(120_000);
    expect(transport.refresh).not.toHaveBeenCalled();
  });

  it.each(["dispose", "stop", "switch"])("cancels pending recovery after %s", async (action) => {
    vi.useFakeTimers();
    const { coordinator, connection } = create();
    transport.ready.mockRejectedValueOnce(new TypeError("offline"));
    await coordinator.connect(bootstrap("http://a"));
    if (action === "dispose") coordinator.dispose();
    else if (action === "stop") coordinator.prepareForNativeRuntimeMutation("stop");
    else await coordinator.connect(bootstrap("http://b"));
    await vi.advanceTimersByTimeAsync(120_000);
    expect(transport.refresh).not.toHaveBeenCalled();
    if (action === "switch") expect(connection.address).toBe("http://b");
  });
});


it("recovers an interrupted event stream without replaying business commands", async () => {
  vi.useFakeTimers();
  const { coordinator, connection } = create();
  let disconnect!: () => void;
  transport.events.mockReturnValueOnce({ closed: new Promise<void>((resolve) => { disconnect = resolve; }) });
  await coordinator.connect(bootstrap("http://a"));
  disconnect();
  await vi.advanceTimersByTimeAsync(1_000);
  expect(connection.state).toBe("connected");
  expect(transport.events).toHaveBeenCalledTimes(2);
  expect(transport.command.mock.calls.every(([, command]) => command.type === "get_application_snapshot")).toBe(true);
});


it.each([0, 150])("replaces old running state after a same-Host user Runtime restart at sequence %s", async (sequence) => {
  vi.useFakeTimers();
  const { coordinator, connection, projection, navigation, live_execution } = create();
  transport.command.mockImplementation(async (address, command) => {
    if (command.type === "get_application_snapshot") {
      const response = result(address);
      response.payload.snapshot.observed_sequence = 100;
      response.payload.snapshot.value.session_groups[0].sessions = [{ ...summary("s"), active_run_id: "r" }];
      return response;
    }
    return sessionResult("s", "before outage", activeRun(), 100);
  });
  await coordinator.connect(bootstrap("http://a"));
  expect(live_execution.runForSession("s")?.status).toBe("running");
  transport.command.mockImplementation(async (address, command) => {
    if (command.type === "get_application_snapshot") {
      const response = result(address);
      response.payload.snapshot.observed_sequence = sequence;
      response.payload.snapshot.value.session_groups[0].sessions = [summary("s")];
      return response;
    }
    return sessionResult("s", "after recovery", null, sequence);
  });
  coordinator.retryConnection();
  await vi.advanceTimersByTimeAsync(1_000);
  expect(connection.state).toBe("connected");
  expect(navigation.selected_session_id).toBe("s");
  expect(projection.session_views.get("s")?.session.title).toBe("after recovery");
  expect(projection.session_views.get("s")?.active_run).toBeNull();
  expect(live_execution.runForSession("s")).toBeNull();
  expect(projection.sessionSnapshotSequence("s")).toBe(sequence);
});


it("keeps center availability independent from local queries and model retries", async () => {
  vi.useFakeTimers();
  const { liveRunStatusLabel } = await import("../../src/features/conversation/ConversationView/conversationRows");
  const { coordinator, connection, projection, live_execution } = create();
  transport.command.mockImplementation(async (address, command) => {
    if (command.type === "get_application_snapshot") {
      const response = result(address);
      response.payload.snapshot.value.session_groups[0].sessions = [{ ...summary("s"), active_run_id: "r" }];
      return response;
    }
    return sessionResult("s", "network test", activeRun());
  });
  await coordinator.connect(bootstrap("http://a"));
  const callbacks = transport.events.mock.calls.at(-1)![1] as { onEvent: (event: unknown) => void; onCenterStatus: (available: boolean) => void };
  callbacks.onCenterStatus(false);
  expect(connection.center_available).toBe(false);
  const emit = (sequence: number, event: unknown) => callbacks.onEvent({ sequence, emitted_at_ms: sequence, event });
  emit(1, { type: "conversation_committed", owner: { type: "main_session", session_id: "s" }, generation: 1 });
  emit(2, { type: "step_committed", owner: { type: "main_session", session_id: "s" }, generation: 1, step: 1 });
  await vi.advanceTimersByTimeAsync(100);
  expect(projection.conversationLoadState("s", null)?.status).toBe("idle");
  emit(3, { type: "model_attempt_failed", session_id: "s", run_id: "r", attempt: 1, kind: "stream_interrupted", will_retry: true });
  emit(4, { type: "model_retry_scheduled", session_id: "s", run_id: "r", next_attempt: 2, delay_ms: 1000 });
  live_execution.flush();
  expect(connection.state).toBe("connected");
  expect(liveRunStatusLabel(live_execution.runForSession("s")!)).toContain("等待 1 秒后进行第 2 次尝试");
  emit(5, { type: "model_attempt_started", session_id: "s", run_id: "r", attempt: 2 });
  live_execution.flush();
  expect(liveRunStatusLabel(live_execution.runForSession("s")!)).toContain("正在重新连接模型 · 第 2 次尝试");
  callbacks.onCenterStatus(true);
  expect(connection.center_available).toBe(true);
  expect(transport.events).toHaveBeenCalledTimes(1);
});
