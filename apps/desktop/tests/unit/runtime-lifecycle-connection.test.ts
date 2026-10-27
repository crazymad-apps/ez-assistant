import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { ConnectionStore } from "../../src/stores/ConnectionStore";
import { NavigationStore } from "../../src/stores/NavigationStore";
import { RuntimeProjectionStore } from "../../src/stores/RuntimeProjectionStore";
import { LiveExecutionStore } from "../../src/stores/LiveExecutionStore";
import type { ApplicationSnapshot, RunSnapshot, SessionSummary } from "@ez-assistant/protocol";
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
    connectEvents(callbacks: unknown) { transport.events(this.address, callbacks); return Promise.resolve({ closed: new Promise<void>(() => undefined) }); }
    waitUntilReady(receive: unknown) { return transport.ready(receive); }
    dispose() {}
  },
}));
const application: ApplicationSnapshot = {
  runtime_lifecycle: "running",
  active_sessions_next_offset: null,
  archived_sessions_next_offset: null, configuration: { config_path: null, revision: "fixture", state: "ready", schema_version: 1, issues: [] }, providers: [], model_settings: { default_model: null, vision_model: null }, workspaces: [], active_sessions: [], archived_sessions: [], controller_availability: { status: "unavailable" }, additional_controller_count: 0,
  capabilities: { conversation_paging: true, mcp_tools: true, mcp_management: true, session_commands: true, tool_detail: true, queue_control: true, approval_queue: true, child_task_view: true, conversation_search: true },
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

describe("bounded owner snapshot reads", () => {
  it("refreshes the loaded list prefixes in one snapshot with authoritative ordering and offsets", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    const loaded = Array.from({ length: 500 }, (_, index) => summary(`old-${index}`));
    projection.appendSessionPage("active", loaded, 500);
    projection.appendSessionPage("archived", Array.from({ length: 235 }, (_, index) => ({ ...summary(`archived-${index}`), lifecycle: "archived" as const })), null);
    transport.command.mockClear();
    transport.command.mockImplementation(async (address, command) => {
      expect(command).toEqual({ type: "get_application_snapshot", payload: { active_sessions_limit: 500, archived_sessions_limit: 235 } });
      const response = result(address);
      response.payload.snapshot.value.active_sessions = [summary("new-pinned"), ...loaded.slice(0, 499)];
      response.payload.snapshot.value.active_sessions_next_offset = 500;
      return response;
    });
    await coordinator.loadApplication();
    expect(transport.command).toHaveBeenCalledOnce();
    expect(projection.application?.active_sessions.map((session) => session.session_id)).toEqual(["new-pinned", ...loaded.slice(0, 499).map((session) => session.session_id)]);
    expect(projection.application?.active_sessions_next_offset).toBe(500);
    expect(projection.application?.archived_sessions).toEqual([]);
    expect(projection.application?.archived_sessions_next_offset).toBeNull();
  });

  it("retries the same enlarged snapshot at most three times without fetching individual pages", async () => {
    const { coordinator, projection, report_error } = create();
    await coordinator.connect(bootstrap("http://a"));
    projection.appendSessionPage("active", [summary("previous")], 500);
    const previous = projection.application;
    vi.useFakeTimers();
    transport.command.mockClear();
    transport.command.mockRejectedValue(new RuntimeClientError("snapshot_busy", "busy"));
    const loading = coordinator.loadApplication();
    await vi.advanceTimersByTimeAsync(1_000);
    await loading;
    expect(transport.command).toHaveBeenCalledTimes(3);
    for (const [, command] of transport.command.mock.calls) {
      expect(command).toEqual({ type: "get_application_snapshot", payload: { active_sessions_limit: 500, archived_sessions_limit: 100 } });
    }
    expect(report_error).toHaveBeenCalledWith("会话暂时繁忙，请重试");
    expect(projection.application).toBe(previous);
  });

  it("does not refetch the application when an older selection stays busy", async () => {
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
    expect(transport.command.mock.calls.filter(([, command]) => command.type === "get_session")).toHaveLength(3);
    expect(report_error).toHaveBeenCalledWith("会话暂时繁忙，请重试");
    expect(projection.application).toBe(previous);
    expect(navigation.selected_session_id).toBe("older");
    expect(connection.state).toBe("connected");
  });

  it("shares one total deadline across the enlarged application and an older selected summary", async () => {
    const { coordinator, projection, navigation, report_error } = create();
    await coordinator.connect(bootstrap("http://a"));
    projection.appendSessionPage("active", [summary("previous")], 500);
    navigation.selectSession("older", false);
    const previous = projection.application;
    vi.useFakeTimers();
    transport.command.mockClear();
    transport.command.mockImplementation((address, command) => {
      if (command.type === "get_application_snapshot") {
        expect(command.payload.active_sessions_limit).toBe(500);
        return new Promise((resolve) => window.setTimeout(() => resolve(result(address)), 14_900));
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
    expect(transport.command).toHaveBeenCalledTimes(2);
    const signals = transport.command.mock.calls.map(([, , options]) => options.signal as AbortSignal);
    expect(signals.every((signal) => signal === signals[0] && signal.aborted)).toBe(true);
    expect(projection.application).toBe(previous);
  });

  it("preserves loaded counts on same-instance reconnect and resets them for a different instance", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    projection.appendSessionPage("active", [summary("previous")], 500);
    transport.command.mockClear();
    await coordinator.connect(bootstrap("http://a"));
    expect(transport.command).toHaveBeenCalledOnce();
    expect(transport.command.mock.calls[0]![1]).toEqual({ type: "get_application_snapshot", payload: { active_sessions_limit: 500, archived_sessions_limit: 100 } });
    projection.appendSessionPage("active", [summary("previous")], 500);
    transport.command.mockClear();
    await coordinator.connect(bootstrap("http://b"));
    expect(transport.command).toHaveBeenCalledOnce();
    expect(transport.command.mock.calls[0]![1]).toEqual({ type: "get_application_snapshot", payload: {} });
  });

  it("preserves a gap rebase requested during an expanded application refresh and resumes the global stream", async () => {
    const { coordinator, projection } = create();
    await coordinator.connect(bootstrap("http://a"));
    projection.appendSessionPage("active", [summary("previous")], 500);
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
      expect(command).toEqual({ type: "get_application_snapshot", payload: { active_sessions_limit: 500, archived_sessions_limit: 100 } });
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
      session_id: "older", title: "Older", model_selection: { provider_instance_id: "provider-1", model_id: "fixture" }, lifecycle: "active", role: "standard",
      current_variant: "build", approval_mode: "ask", workspace_id: null, active_run_id: null,
      message_count: 0, queued_input_count: 0, resume_required: false, created_at_ms: 1, updated_at_ms: 1,
      archived_at_ms: null, is_pinned: false, title_origin: "user", pending_approval_count: 0,
      active_child_count: 0, active_run_status: null,
    };
    transport.command.mockImplementation(async (address: string, command: { type: string }) => {
      if (command.type === "get_session") return { payload: { session: older } };
      const response = result(address);
      response.payload.snapshot.value.active_sessions_next_offset = 100;
      return response;
    });
    // 避开本用例不关心的正文装配，仅验证连接后的摘要与选择恢复。
    vi.spyOn(coordinator, "loadSession").mockResolvedValue();
    coordinator.retryConnection();
    await vi.waitFor(() => expect(projection.application?.active_sessions).toContainEqual(older));
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
