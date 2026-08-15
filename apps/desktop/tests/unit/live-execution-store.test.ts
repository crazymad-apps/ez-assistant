import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  RuntimeEvent,
  RuntimeEventEnvelope,
  SessionViewSnapshot,
} from "../../src/generated/assistant-protocol";
import { LiveExecutionStore } from "../../src/stores/LiveExecutionStore";

describe("LiveExecutionStore", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("batches deltas until the next animation frame", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    const event = {
      sequence: 1,
      emitted_at_ms: 1,
      event: {
        type: "text_delta" as const,
        session_id: "session-1",
        run_id: "run-1",
        part_id: "part-1",
        delta: "hello",
      },
    };

    store.buffer(event);
    store.buffer({ ...event, sequence: 2, event: { ...event.event, delta: " world" } });
    expect(store.runs.size).toBe(0);
    const callback = frame as FrameRequestCallback | null;
    expect(callback).not.toBeNull();
    callback?.(0);
    const run = store.runForSession("session-1");
    expect(run?.steps[0]?.segments).toEqual([
      { type: "text", part_id: "part-1", text: "hello world" },
    ]);
  });

  it("exposes a child task as soon as its created event is flushed", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();

    store.buffer(envelope(1, {
      type: "child_task_event",
      session_id: "session-1",
      parent_run_id: "run-parent",
      child_task_id: "child-live",
      event: {
        type: "created",
        task: {
          child_task_id: "child-live",
          session_id: "session-1",
          parent_run_id: "run-parent",
          parent_tool_call_id: "call-delegate",
          title: "实时子任务",
          status: "accepted",
          variant: "build",
          cancel_requested: false,
          final_text: "",
          error: null,
          created_at_ms: 1,
          started_at_ms: null,
          finished_at_ms: null,
        },
      },
    }));

    const callback = frame as FrameRequestCallback | null;
    callback?.(0);
    expect(store.childTasksForSession("session-1")).toHaveLength(1);
    expect(store.childTasksForSession("session-1")[0]?.title).toBe("实时子任务");
  });

  it("preserves reasoning, text and tool groups in event order", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();

    store.buffer(envelope(1, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "reasoning-1",
      delta: "先检查",
    }));
    store.buffer(envelope(2, {
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "text-1",
      delta: "开始读取。",
    }));
    store.buffer(envelope(3, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      call_id: "call-1",
      tool_name: "read_file",
    }));
    store.buffer(envelope(4, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "reasoning-2",
      delta: "继续核对",
    }));

    const callback = frame as FrameRequestCallback | null;
    expect(callback).not.toBeNull();
    callback?.(0);
    expect(store.runForSession("session-1")?.steps[0]?.segments.map((segment) => segment.type)).toEqual([
      "reasoning",
      "text",
      "tool_group",
      "reasoning",
    ]);
  });

  it("keeps reused part ids isolated by model step", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();

    store.buffer(envelope(1, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
    }));
    store.buffer(envelope(2, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "part-1",
      delta: "第一步",
    }));
    store.buffer(envelope(3, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 2,
    }));
    store.buffer(envelope(4, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "part-1",
      delta: "第二步",
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    const run = store.runForSession("session-1");
    expect(run?.steps.map((step) => step.segments)).toEqual([
      [{ type: "reasoning", part_id: "part-1", text: "第一步" }],
      [{ type: "reasoning", part_id: "part-1", text: "第二步" }],
    ]);
  });

  it("keeps one model-step tool batch in one group", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();

    store.buffer(envelope(1, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
    }));
    store.buffer(envelope(2, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      call_id: "call-1",
      tool_name: "read_file",
    }));
    store.buffer(envelope(3, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      call_id: "call-2",
      tool_name: "search_content",
    }));
    store.buffer(envelope(4, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 2,
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    const run = store.runForSession("session-1");
    expect(run?.steps[0]?.segments).toEqual([{
      type: "tool_group",
      group_id: "live-tools:call-1",
      tools: [
        { call_id: "call-1", tool_name: "read_file", status: "proposed", stdout: "", stderr: "" },
        { call_id: "call-2", tool_name: "search_content", status: "proposed", stdout: "", stderr: "" },
      ],
    }]);
    expect(run?.steps[1]?.segments).toEqual([]);
  });

  it("moves committed steps out of the live projection without re-adding their tools", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
    }));
    store.buffer(envelope(2, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      call_id: "call-1",
      tool_name: "read_file",
    }));
    store.buffer(envelope(3, {
      type: "step_started",
      session_id: "session-1",
      run_id: "run-1",
      step: 2,
    }));
    store.buffer(envelope(4, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "part-1",
      delta: "继续",
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: {
        run_id: "run-1",
        session_id: "session-1",
        status: "running",
        reasoning: "继续",
        text: "",
        tools: [{
          call_id: "call-1",
          tool_name: "read_file",
          status: "completed",
          stdout: "done",
          stderr: "",
        }],
      },
      conversation: {
        items: [{
          type: "assistant",
          run_id: "run-1",
          segments: [{
            type: "tool_group",
            tools: [{ call_id: "call-1", tool_name: "read_file", status: "completed", summary: null }],
          }],
        }],
      },
    } as unknown as SessionViewSnapshot);

    expect(store.runForSession("session-1")?.steps).toEqual([
      { step: 2, segments: [{ type: "reasoning", part_id: "part-1", text: "继续" }] },
    ]);
  });

  it("removes a live projection after its assistant message is committed", () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "text-1",
      delta: "done",
    }));

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      conversation: {
        items: [{ type: "assistant", run_id: "run-1", segments: [] }],
      },
    } as unknown as SessionViewSnapshot);

    expect(store.runForSession("session-1")).toBeNull();
  });

  it("drops a stale running projection when the authoritative view has no active run", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "run_started",
      session_id: "session-1",
      run_id: "run-1",
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      conversation: { items: [] },
    } as unknown as SessionViewSnapshot);

    expect(store.runForSession("session-1")).toBeNull();
  });

  it("keeps the terminal projection until the reliable session view replaces it", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      part_id: "text-1",
      delta: "done",
    }));
    store.buffer(envelope(2, {
      type: "run_finished",
      session_id: "session-1",
      run_id: "run-1",
      status: "completed",
      error: null,
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForSession("session-1")?.status).toBe("completed");

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      conversation: { items: [{ type: "assistant", run_id: "run-1", segments: [] }] },
    } as unknown as SessionViewSnapshot);
    expect(store.runForSession("session-1")).toBeNull();
  });

  it("applies buffered terminal events before reconciling a newer session view", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "run_finished",
      session_id: "session-1",
      run_id: "run-1",
      status: "completed",
      error: null,
    }));

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      conversation: { items: [{ type: "assistant", run_id: "run-1", segments: [] }] },
    } as unknown as SessionViewSnapshot);
    expect(store.runForSession("session-1")).toBeNull();

    const stale_callback = frame as FrameRequestCallback | null;
    stale_callback?.(0);
    expect(store.runForSession("session-1")).toBeNull();
  });
});

function envelope(sequence: number, event: RuntimeEvent): RuntimeEventEnvelope {
  return { sequence, emitted_at_ms: sequence, event };
}
