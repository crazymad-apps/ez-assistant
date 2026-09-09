import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { RuntimeLifecycleCoordinator } from "../../src/stores/RuntimeLifecycleCoordinator";
import { ConnectionStore } from "../../src/stores/ConnectionStore";
import { NavigationStore } from "../../src/stores/NavigationStore";
import { RuntimeProjectionStore } from "../../src/stores/RuntimeProjectionStore";
import { LiveExecutionStore } from "../../src/stores/LiveExecutionStore";
import type { ApplicationSnapshot, SessionSummary } from "../../src/generated/assistant-protocol";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";

const transport = vi.hoisted(() => ({ command: vi.fn(), events: vi.fn(), ready: vi.fn(), bootstrap: vi.fn(), refresh: vi.fn() }));
vi.mock("../../src/native-bridge/runtimeBootstrap", () => ({ bootstrapRuntime: transport.bootstrap }));
vi.mock("../../src/native-bridge/runtimeConnection", () => ({ refreshRuntimeConnection: transport.refresh }));
vi.mock("../../src/runtime-client/RuntimeClient", () => ({
  RuntimeClientError: class extends Error {},
  RuntimeClient: class {
    instance_id: string; address: string; capabilities: RuntimeBootstrap["capabilities"];
    constructor(bootstrap: RuntimeBootstrap) { this.instance_id = bootstrap.instance_id; this.address = bootstrap.base_url; this.capabilities = bootstrap.capabilities; }
    command(command: unknown) { return transport.command(this.address, command); }
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
const bootstrap = (origin: string): RuntimeBootstrap => ({ base_url: origin, instance_id: origin, binding_id: origin, target_kind: "remote", access_token: "fixture", capabilities: { protocol_version: 3, runtime_version: "0.24.0", max_command_bytes: 1048576, max_attachment_bytes: null, sse: true, streaming_upload: true, features: ["web_login", "startup_diagnostics"] }, started_runtime: false });
const result = (revision: string) => ({ type: "get_application_snapshot", payload: { snapshot: { observed_sequence: 0, value: { ...structuredClone(application), configuration: { ...application.configuration, revision } } } } });
const owners: RuntimeLifecycleCoordinator[] = [];
function create() {
  const connection = new ConnectionStore(), projection = new RuntimeProjectionStore(), navigation = new NavigationStore();
  const coordinator = new RuntimeLifecycleCoordinator({ connection, projection, navigation, live_execution: new LiveExecutionStore(), report_interaction_error: vi.fn(), refresh_device_gateway: vi.fn(), mark_device_gateway_stale: vi.fn(), on_title_generation_finished: vi.fn() });
  owners.push(coordinator); return { coordinator, connection, projection, navigation };
}
beforeEach(() => { vi.clearAllMocks(); transport.command.mockImplementation(async (address: string) => result(address)); transport.refresh.mockImplementation(async (value: RuntimeBootstrap) => value); });
afterEach(() => { owners.splice(0).forEach((owner) => owner.dispose()); vi.restoreAllMocks(); });

describe("selected Runtime lifecycle", () => {
  it("refreshes application capabilities when background MCP discovery completes", async () => {
    const { coordinator } = create();
    await coordinator.connect(bootstrap("http://a"));
    const before = transport.command.mock.calls.length;
    const callbacks = transport.events.mock.calls.at(-1)![1];
    callbacks.onEvent({ sequence: 1, emitted_at_ms: 1, event: { type: "mcp_registry_changed" } });
    await vi.waitFor(() => expect(transport.command.mock.calls.length).toBeGreaterThan(before));
    expect(transport.command.mock.calls.at(-1)![1].type).toBe("get_application_snapshot");
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
