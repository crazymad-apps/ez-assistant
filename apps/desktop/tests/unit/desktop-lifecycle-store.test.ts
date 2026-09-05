import { ResourceWorkspaceStore } from "../../src/features/resource-workspace/ResourceWorkspaceStore";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApplicationSnapshot } from "../../src/generated/assistant-protocol";
import type {
  DesktopLifecycleIntent,
  NativeRuntimeMutationEvent,
} from "../../src/native-bridge/desktopLifecycle";
import type { RuntimeBootstrap } from "../../src/native-bridge/runtimeBootstrap";

const native = vi.hoisted(() => ({
  listen: vi.fn(async () => () => undefined),
  listen_runtime_mutations: vi.fn(
    async (listener: (event: NativeRuntimeMutationEvent) => void) => {
      native.runtime_mutation_listener = listener;
      return () => undefined;
    },
  ),
  quit: vi.fn(async () => undefined),
  restart: vi.fn<() => Promise<RuntimeBootstrap | null>>(async () => null),
  stop: vi.fn(async () => undefined),
  take_pending: vi.fn<() => Promise<DesktopLifecycleIntent | null>>(async () => null),
  update: vi.fn(async (_state: string) => undefined),
  shutdown_terminals: vi.fn(async () => undefined),
  resume_terminals: vi.fn(async () => undefined),
  runtime_mutation_listener: null as ((event: NativeRuntimeMutationEvent) => void) | null,
}));

vi.mock("../../src/native-bridge/userTerminal", () => ({
  shutdownUserTerminals: native.shutdown_terminals,
  resumeUserTerminals: native.resume_terminals,
}));

vi.mock("../../src/native-bridge/desktopLifecycle", () => ({
  listenDesktopLifecycleIntents: native.listen,
  listenNativeRuntimeMutations: native.listen_runtime_mutations,
  quitDesktopClient: native.quit,
  restartNativeRuntime: native.restart,
  stopNativeRuntime: native.stop,
  takePendingDesktopLifecycleIntent: native.take_pending,
  updateNativeRuntimeState: native.update,
}));

import { DesktopLifecycleStore } from "../../src/stores/DesktopLifecycleStore";

beforeEach(() => {
  vi.clearAllMocks();
  native.take_pending.mockResolvedValue(null);
  native.runtime_mutation_listener = null;
});

describe("DesktopLifecycleStore", () => {
  it("summarizes the exact runtime impact before confirming a lifecycle action", () => {
    const store = createStore(applicationSnapshot());

    expect(store.impact).toEqual({
      active_runs: 1,
      queued_inputs: 3,
      pending_approvals: 2,
    });
  });

  it("quits only the desktop client unless stopping Runtime is explicitly selected", async () => {
    const store = createStore(null);
    store.request("quit_desktop");

    await store.confirm();

    expect(native.quit).toHaveBeenCalledOnce();
    expect(native.stop).not.toHaveBeenCalled();
  });

  it("restarts Runtime through the mutation and reconnect sequence", async () => {
    const steps: string[] = [];
    const bootstrap: RuntimeBootstrap = {
      base_url: "http://127.0.0.1:41000",
      instance_id: "runtime-next",
      access_token: "secret",
      capabilities: {} as RuntimeBootstrap["capabilities"],
      started_runtime: true,
    };
    native.update.mockImplementation(async (state: string) => {
      steps.push(`native:${state}`);
    });
    native.restart.mockImplementation(async () => {
      steps.push("native:restart");
      return bootstrap;
    });
    const store = createStore(null, {
      prepare_runtime_mutation: (kind) => steps.push(`prepare:${kind}`),
      reconnect_runtime: async (received) => {
        expect(received).toBe(bootstrap);
        steps.push("reconnect");
      },
    });
    store.request("restart_runtime");

    await store.confirm();

    expect(steps).toEqual([
      "prepare:restart",
      "native:restarting",
      "native:restart",
      "reconnect",
    ]);
    expect(store.intent).toBeNull();
    expect(store.pending).toBe(false);
  });

  it("persists close behavior without coupling it to a Runtime stop", () => {
    const save_preferences = vi.fn();
    const store = createStore(null, { save_preferences });

    store.setCloseBehavior("quit_desktop");

    expect(store.close_behavior).toBe("quit_desktop");
    expect(save_preferences).toHaveBeenCalledOnce();
    expect(native.stop).not.toHaveBeenCalled();
  });

  it("claims a durable native intent when a hidden window becomes active", async () => {
    native.take_pending.mockResolvedValueOnce("restart_runtime");
    const store = createStore(null);

    store.start();
    await vi.waitFor(() => expect(store.intent).toBe("restart_runtime"));

    store.dispose();
  });

  it("coordinates a native tray restart without claiming a WebView intent", async () => {
    const prepare_runtime_mutation = vi.fn();
    const reconnect_runtime = vi.fn(async () => undefined);
    const store = createStore(null, { prepare_runtime_mutation, reconnect_runtime });
    store.start();
    await vi.waitFor(() => expect(native.runtime_mutation_listener).not.toBeNull());

    native.runtime_mutation_listener?.({ phase: "preparing", kind: "restart" });
    native.runtime_mutation_listener?.({ phase: "finished", kind: "restart", succeeded: true });

    expect(prepare_runtime_mutation).toHaveBeenCalledWith("restart");
    await vi.waitFor(() => expect(reconnect_runtime).toHaveBeenCalledOnce());
    expect(store.intent).toBeNull();
    store.dispose();
  });

  it("marks a native tray stop without reconnecting the stopped Runtime", async () => {
    const mark_runtime_stopped = vi.fn();
    const reconnect_runtime = vi.fn(async () => undefined);
    const store = createStore(null, { mark_runtime_stopped, reconnect_runtime });
    store.start();
    await vi.waitFor(() => expect(native.runtime_mutation_listener).not.toBeNull());

    native.runtime_mutation_listener?.({ phase: "preparing", kind: "stop" });
    native.runtime_mutation_listener?.({ phase: "finished", kind: "stop", succeeded: true });

    expect(mark_runtime_stopped).toHaveBeenCalledOnce();
    expect(reconnect_runtime).not.toHaveBeenCalled();
    expect(store.intent).toBeNull();
    store.dispose();
  });
});

type Overrides = Partial<ConstructorParameters<typeof DesktopLifecycleStore>[0]>;

function createStore(application: ApplicationSnapshot | null, overrides: Overrides = {}) {
  return new DesktopLifecycleStore({
    resources: new ResourceWorkspaceStore(),
    get_application: () => application,
    prepare_runtime_mutation: () => undefined,
    reconnect_runtime: async () => undefined,
    mark_runtime_stopped: () => undefined,
    save_preferences: () => undefined,
    ...overrides,
  });
}

function applicationSnapshot(): ApplicationSnapshot {
  return {
    active_sessions: [
      {
        active_run_id: "run-1",
        queued_input_count: 2,
        pending_approval_count: 1,
      },
      {
        active_run_id: null,
        queued_input_count: 1,
        pending_approval_count: 1,
      },
    ],
  } as ApplicationSnapshot;
}

it("waits for terminal cleanup before stopping Runtime or exiting and aborts on cleanup failure", async () => {
  const resources = new ResourceWorkspaceStore();
  let complete!: () => void;
  vi.spyOn(resources, "shutdownTerminals").mockImplementationOnce(() => new Promise<void>((resolve) => { complete = resolve; }))
    .mockRejectedValueOnce(new Error("terminal cleanup failed"));
  const store = createStore(null, { resources });
  store.request("quit_desktop");
  store.setStopRuntimeOnQuit(true);
  const closing = store.confirm();
  expect(native.stop).not.toHaveBeenCalled();
  expect(native.quit).not.toHaveBeenCalled();
  complete();
  await closing;
  expect(native.stop).toHaveBeenCalledOnce();
  expect(native.quit).toHaveBeenCalledOnce();
  const retry = createStore(null, { resources });
  native.stop.mockClear(); native.quit.mockClear();
  retry.request("quit_desktop"); retry.setStopRuntimeOnQuit(true);
  await retry.confirm();
  expect(native.stop).not.toHaveBeenCalled();
  expect(native.quit).not.toHaveBeenCalled();
  expect(retry.pending).toBe(false);
  expect(retry.error_message).toBe("terminal cleanup failed");
});

it("allows retrying a failed quit even if restoring the native terminal service also fails", async () => {
  const resources = new ResourceWorkspaceStore();
  const store = createStore(null, { resources });
  native.quit.mockRejectedValueOnce(new Error("quit failed"));
  native.resume_terminals.mockRejectedValueOnce(new Error("native bridge unavailable"));
  store.request("quit_desktop");

  await store.confirm();

  expect(store.pending).toBe(false);
  expect(resources.shutting_down).toBe(true);
  expect(store.error_message).toContain("quit failed");
  expect(store.error_message).toContain("终端服务尚未恢复");
  await store.confirm();
  expect(native.quit).toHaveBeenCalledTimes(2);
});
