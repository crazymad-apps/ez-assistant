import { emptyRun } from "../../src/stores/liveExecutionProjection";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  ChildTaskViewSnapshot,
  RuntimeEvent,
  RuntimeEventEnvelope,
  SessionViewSnapshot,
} from "@ez-assistant/protocol";
import { executionElapsedMs, hostClockAnchor, restartStep } from "../../src/stores/liveExecutionProjection";
import { LiveExecutionStore } from "../../src/stores/LiveExecutionStore";

describe("LiveExecutionStore", () => {
  it("rebases child metadata without repeating a displayed delta or reverting a later failure", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const live = new LiveExecutionStore();
    const view: ChildTaskViewSnapshot = {
      task: { task: { session_id: "session-1", child_task_id: "child", parent_run_id: "run-1", parent_tool_call_id: "call",
        title: "child", status: "running", variant: "build", cancel_requested: false, final_text: "", error: null,
        created_at_ms: 1, started_at_ms: 2, finished_at_ms: null, inherited: false, body_unavailable: false },
      usage: { accumulated: null }, pending_approval_count: 0, can_cancel: true },
      approval_ids: [], conversation: { owner: { type: "child_task", session_id: "session-1", child_task_id: "child" },
        generation: 1, items: [], previous_cursor: null, has_more: false },
    };
    const text = envelope(1, { type: "child_task_event", session_id: "session-1", parent_run_id: "run-1", child_task_id: "child",
      event: { type: "text_delta", step: 1, part_id: "p", delta: "tail" } });
    const failed = envelope(2, { type: "child_task_event", session_id: "session-1", parent_run_id: "run-1", child_task_id: "child",
      event: { type: "finished", status: "failed", error: { code: "model_execution_failed", message: "later" }, finished_at_ms: 3 } });
    live.buffer(text); live.buffer(failed);
    live.rebaseChildTask(view, 2, [text, failed]);
    const run = live.runForChildTask("session-1", "child");
    expect(run?.status).toBe("failed");
    expect(run?.steps[0]?.segments).toMatchObject([{ type: "text", text: "tail" }]);
    expect(live.childTasksForSession("session-1")[0]?.status).toBe("failed");
  });

  it("preserves a later child across a parent baseline while keeping inherited descriptors authoritative", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const live = new LiveExecutionStore();
    const task: ChildTaskViewSnapshot["task"]["task"] = {
      session_id: "session-1", child_task_id: "child", parent_run_id: "run-1", parent_tool_call_id: "call", title: "child",
      status: "running", variant: "build", cancel_requested: false, final_text: "", error: null,
      created_at_ms: 1, started_at_ms: 2, finished_at_ms: null, inherited: false, body_unavailable: false,
    };
    const created = envelope(2, { type: "child_task_event", session_id: "session-1", parent_run_id: "run-1", child_task_id: "child", event: { type: "created", task } });
    const text = envelope(3, { type: "child_task_event", session_id: "session-1", parent_run_id: "run-1", child_task_id: "child", event: { type: "text_delta", step: 1, part_id: "p", delta: "tail" } });
    live.buffer(created); live.buffer(text);
    const view = activeSessionView(1);
    view.child_tasks = [];
    live.rebaseSession(view, 1, 1, [created, text]);
    expect(live.childTasksForSession("session-1")).toHaveLength(1);
    expect(live.runForChildTask("session-1", "child")?.steps[0]?.segments).toMatchObject([{ text: "tail" }]);
    view.child_tasks = [{ task: { ...task, inherited: true, status: "completed" }, usage: { accumulated: null }, pending_approval_count: 0, can_cancel: false }];
    live.rebaseSession(view, 1, 1, [created, text]);
    expect(live.childTasksForSession("session-1")[0]?.inherited).toBe(true);
    expect(live.runForChildTask("session-1", "child")).toBeNull();
  });

  it("keeps source live output separate from an inherited child with the same ID", () => {
    vi.stubGlobal("requestAnimationFrame", () => 1);
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const live = new LiveExecutionStore();
    const view: ChildTaskViewSnapshot = {
      task: { task: {
        session_id: "branch", child_task_id: "shared", parent_run_id: "parent", parent_tool_call_id: "call",
        title: "inherited", status: "completed", variant: "build", cancel_requested: false,
        final_text: "frozen", error: null, created_at_ms: 1, started_at_ms: 2, finished_at_ms: 3,
        inherited: true, body_unavailable: false,
      }, usage: { accumulated: null }, pending_approval_count: 0, can_cancel: false },
      approval_ids: [],
      conversation: { owner: { type: "child_task", session_id: "branch", child_task_id: "shared" },
        generation: 1, items: [], previous_cursor: null, has_more: false },
    };
    live.buffer(envelope(1, { type: "child_task_event", session_id: "source", parent_run_id: "parent",
      child_task_id: "shared", event: { type: "text_delta", step: 1, part_id: "text", delta: "source" } }));
    live.reconcileChildTask(view);
    live.buffer(envelope(2, { type: "child_task_event", session_id: "branch", parent_run_id: "parent",
      child_task_id: "shared", event: { type: "text_delta", step: 1, part_id: "text", delta: "must not run" } }));
    live.buffer(envelope(3, { type: "step_committed", owner: view.conversation.owner, generation: 1, step: 1 }));
    live.flush();
    expect(live.runForChildTask("source", "shared")?.steps[0]?.segments[0]).toMatchObject({ text: "source" });
    expect(live.runForChildTask("branch", "shared")).toBeNull();
    live.reconcileChildTask({ ...view, conversation: { ...view.conversation, generation: 2 } });
    expect(live.runForChildTask("source", "shared")?.steps).toHaveLength(1);
    live.dispose();
  });

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
        step: 1,
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
          inherited: false,
          body_unavailable: false,
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

  it("removes child task projections absent from the authoritative session view", () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "child_task_event",
      session_id: "session-1",
      parent_run_id: "run-parent",
      child_task_id: "child-stale",
      event: {
        type: "created",
        task: {
          inherited: false,
          body_unavailable: false,
          child_task_id: "child-stale",
          session_id: "session-1",
          parent_run_id: "run-parent",
          parent_tool_call_id: "call-delegate",
          title: "待清理子任务",
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

    expect(store.childTasksForSession("session-1")).toHaveLength(1);
    expect(store.runForChildTask("session-1", "child-stale")).not.toBeNull();

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      child_tasks: [],
      conversation: { items: [] },
    } as unknown as SessionViewSnapshot);

    expect(store.childTasksForSession("session-1")).toHaveLength(0);
    expect(store.runForChildTask("session-1", "child-stale")).toBeNull();
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
      step: 1,
      part_id: "reasoning-1",
      delta: "先检查",
    }));
    store.buffer(envelope(2, {
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
      part_id: "text-1",
      delta: "开始读取。",
    }));
    store.buffer(envelope(3, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
      call_id: "call-1",
      tool_name: "read_file",
      input: toolInput(),
    }));
    store.buffer(envelope(4, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
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
      step: 1,
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
      step: 2,
      part_id: "part-1",
      delta: "第二步",
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    const run = store.runForSession("session-1");
    expect(run?.steps.map((step) => step.segments)).toEqual([
      [{ type: "reasoning", source: "raw", part_id: "part-1", text: "第一步" }],
      [{ type: "reasoning", source: "raw", part_id: "part-1", text: "第二步" }],
    ]);
  });

  it("keeps the read_image follow-up step isolated when StepStarted is dropped", () => {
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
      run_id: "run-image",
      step: 1,
    }));
    store.buffer(envelope(2, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-image",
      part_id: "reasoning-1",
      delta: "先读取图片",
      step: 1,
    }));
    store.buffer(envelope(3, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-image",
      call_id: "call-read-image",
      tool_name: "read_image",
      input: toolInput(),
      step: 1,
    }));

    // 模拟允许丢弃的 StepStarted 未送达。后续事件必须能仅凭自身 step 定位，
    // 不能依赖工具名、当前活动容器或可靠 Assistant 消息数量猜测归属。
    store.buffer(envelope(4, {
      type: "reasoning_delta",
      session_id: "session-1",
      run_id: "run-image",
      part_id: "reasoning-1",
      delta: "图片中存在一张表格",
      step: 2,
    }));
    store.buffer(envelope(5, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-image",
      call_id: "call-next",
      tool_name: "read_file",
      input: toolInput(),
      step: 2,
    }));

    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForSession("session-1")?.steps).toEqual([
      {
        step: 1,
        segments: [
          { type: "reasoning", source: "raw", part_id: "reasoning-1", text: "先读取图片" },
          {
            type: "tool_group",
            group_id: "live-tools:call-read-image",
            tools: [{
              call_id: "call-read-image",
              tool_name: "read_image",
              input: toolInput(),
              status: "proposed",
              stdout: "",
              stderr: "",
            }],
          },
        ],
      },
      {
        step: 2,
        segments: [
          { type: "reasoning", source: "raw", part_id: "reasoning-1", text: "图片中存在一张表格" },
          {
            type: "tool_group",
            group_id: "live-tools:call-next",
            tools: [{
              call_id: "call-next",
              tool_name: "read_file",
              input: toolInput(),
              status: "proposed",
              stdout: "",
              stderr: "",
            }],
          },
        ],
      },
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
      step: 1,
      call_id: "call-1",
      tool_name: "read_file",
      input: toolInput(),
    }));
    store.buffer(envelope(3, {
      type: "tool_proposed",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
      call_id: "call-2",
      tool_name: "search_content",
      input: toolInput(),
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
        { call_id: "call-1", tool_name: "read_file", input: toolInput(), status: "proposed", stdout: "", stderr: "" },
        { call_id: "call-2", tool_name: "search_content", input: toolInput(), status: "proposed", stdout: "", stderr: "" },
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
      step: 1,
      call_id: "call-1",
      tool_name: "read_file",
      input: toolInput(),
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
      step: 2,
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
        active_step: 2,
        reasoning_parts: [{ part_id: "reasoning-2", text: "继续" }],
        text: "",
        tools: [{
          step: 1,
          call_id: "call-1",
          tool_name: "read_file",
          input: toolInput(),
          status: "completed",
          stdout: "done",
          stderr: "",
        }],
      },
      conversation: {
        items: [{
          type: "assistant",
          run_id: "run-1",
          step: 1,
          segments: [{
            type: "tool_group",
            tools: [{ call_id: "call-1", tool_name: "read_file", status: "completed", summary: null, input: toolInput() }],
          }],
        }],
      },
    } as unknown as SessionViewSnapshot);

    expect(store.runForSession("session-1")?.steps).toEqual([
      { step: 2, segments: [{ type: "reasoning", source: "raw", part_id: "part-1", text: "继续" }] },
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
      step: 1,
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
      step: 1,
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

  it("keeps a terminal error visible until the next run starts", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "model_attempt_failed",
      session_id: "session-1",
      run_id: "run-1",
      attempt: 1,
      kind: "authentication",
      will_retry: false,
    }));
    store.buffer(envelope(1, {
      type: "run_finished",
      session_id: "session-1",
      run_id: "run-1",
      status: "failed",
      error: { code: "model_execution_failed", message: "API Key 无效" },
    }));
    const failed_callback = frame as FrameRequestCallback | null;
    failed_callback?.(0);

    store.reconcileSession({
      session: { session_id: "session-1" },
      active_run: null,
      conversation: { items: [] },
    } as unknown as SessionViewSnapshot);

    expect(store.runForSession("session-1")).toMatchObject({
      run_id: "run-1",
      status: "failed",
      error_code: "model_execution_failed",
      error_message: "API Key 无效",
      model_failure_kind: "authentication",
    });

    store.buffer(envelope(2, {
      type: "run_accepted",
      session_id: "session-1",
      run_id: "run-2",
    }));
    const accepted_callback = frame as FrameRequestCallback | null;
    accepted_callback?.(0);

    expect(store.runForSession("session-1")?.run_id).toBe("run-2");
    expect(store.runs.size).toBe(1);
  });

  it("clears a retried model failure once the stream is established", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "model_attempt_failed",
      session_id: "session-1",
      run_id: "run-1",
      attempt: 1,
      kind: "connection",
      will_retry: true,
    }));
    store.buffer(envelope(2, {
      type: "model_stream_established",
      session_id: "session-1",
      run_id: "run-1",
      attempt: 2,
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForSession("session-1")?.model_failure_kind).toBeNull();
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

  it("seals a committed main step and rejects every late event for that step", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.reconcileSession(activeSessionView(7));

    store.buffer(envelope(1, {
      type: "text_delta", session_id: "session-1", run_id: "run-1", step: 1,
      part_id: "text-1", delta: "可靠提交前",
    }));
    store.buffer(envelope(2, {
      type: "step_committed",
      owner: { type: "main_session", session_id: "session-1" },
      step: 1,
      generation: 7,
    }));
    store.buffer(envelope(3, {
      type: "text_delta", session_id: "session-1", run_id: "run-1", step: 1,
      part_id: "text-1", delta: "迟到内容",
    }));
    store.buffer(envelope(4, {
      type: "tool_proposed", session_id: "session-1", run_id: "run-1", step: 1,
      call_id: "late-tool", tool_name: "read_file", input: toolInput(),
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForSession("session-1")?.steps).toEqual([]);
  });

  it("invalidates a main live tail when StepCommitted generation cannot match", () => {
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.reconcileSession(activeSessionView(3));
    store.buffer(envelope(1, {
      type: "step_committed",
      owner: { type: "main_session", session_id: "session-1" },
      step: 1,
      generation: 2,
    }));

    expect(store.runForSession("session-1")).toBeNull();
  });

  it("rejects late step events after a run becomes terminal", () => {
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
    store.buffer(envelope(2, {
      type: "text_delta",
      session_id: "session-1",
      run_id: "run-1",
      step: 1,
      part_id: "late-text",
      delta: "迟到内容",
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForSession("session-1")?.status).toBe("completed");
    expect(store.runForSession("session-1")?.steps).toEqual([]);
  });

  it("seals child steps independently from the parent run", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.reconcileChildTask({
      task: { task: {
        inherited: false,
        body_unavailable: false,
        child_task_id: "child-1", session_id: "session-1", parent_run_id: "run-1",
        parent_tool_call_id: "delegate-1", title: "child", status: "running", variant: "build",
        cancel_requested: false, final_text: "", error: null, created_at_ms: 1,
        started_at_ms: 1, finished_at_ms: null,
      } },
      conversation: {
        owner: { type: "child_task", session_id: "session-1", child_task_id: "child-1" },
        generation: 5,
        items: [],
      },
    } as unknown as ChildTaskViewSnapshot);
    store.buffer(envelope(1, {
      type: "child_task_event", session_id: "session-1", parent_run_id: "run-1",
      child_task_id: "child-1", event: { type: "text_delta", step: 1, part_id: "part-1", delta: "before" },
    }));
    store.buffer(envelope(2, {
      type: "step_committed",
      owner: { type: "child_task", session_id: "session-1", child_task_id: "child-1" },
      step: 1,
      generation: 5,
    }));
    store.buffer(envelope(3, {
      type: "child_task_event", session_id: "session-1", parent_run_id: "run-1",
      child_task_id: "child-1", event: { type: "text_delta", step: 1, part_id: "part-1", delta: "late" },
    }));
    const callback = frame as FrameRequestCallback | null;
    callback?.(0);

    expect(store.runForChildTask("session-1", "child-1")?.steps).toEqual([]);
  });

  it("drops pending and live tails synchronously when a stream gap is observed", () => {
    let frame: FrameRequestCallback | null = null;
    vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
      frame = callback;
      return 1;
    });
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    store.buffer(envelope(1, {
      type: "text_delta", session_id: "session-1", run_id: "run-1", step: 2,
      part_id: "part-2", delta: "gap-left",
    }));

    store.invalidateForGap();
    const stale_callback = frame as FrameRequestCallback | null;
    stale_callback?.(0);

    expect(store.runs.size).toBe(0);
    expect(store.child_runs.size).toBe(0);
  });
});

function envelope(sequence: number, event: RuntimeEvent): RuntimeEventEnvelope {
  return { sequence, emitted_at_ms: sequence, event };
}

function toolInput() {
  return { value: { type: "unavailable" as const }, redacted: false, truncated: false };
}

function activeSessionView(generation: number): SessionViewSnapshot {
  return {
    session: { session_id: "session-1" },
    active_run: {
      run_id: "run-1",
      session_id: "session-1",
      status: "running",
      active_step: 1,
      reasoning_parts: [],
      text: "",
      tools: [],
    },
    child_tasks: [],
    conversation: {
      owner: { type: "main_session", session_id: "session-1" },
      generation,
      items: [],
    },
  } as unknown as SessionViewSnapshot;
}

describe("model request progress", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("recovers the latest persisted failure after reload and drops it when a newer run succeeds", () => {
    const store = new LiveExecutionStore();
    const view = activeSessionView(1);
    const failed = { ...view.active_run!, status: "failed" as const, error: { code: "model_execution_failed" as const, message: "safe failure" } };
    store.reconcileSession({ ...view, active_run: null, runs: [failed] });
    expect(store.runForSession("session-1")?.error_message).toBe("safe failure");
    store.reconcileSession({ ...view, active_run: null, runs: [failed, { ...failed, run_id: "run-2", status: "completed", error: null }] });
    expect(store.runForSession("session-1")).toBeNull();
  });

  it("restores retry waits from snapshots and clears them on success, cancellation and terminal events", () => {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    const store = new LiveExecutionStore();
    const view = activeSessionView(1);
    view.active_run!.model_request = { type: "retry_waiting", next_attempt: 2, delay_ms: 2000 };
    store.reconcileSession(view);
    expect(store.runForSession("session-1")?.model_request).toEqual(view.active_run!.model_request);
    store.invalidateForGap();
    expect(store.runForSession("session-1")).toBeNull();
    store.reconcileSession(view);
    const send = (event: RuntimeEvent) => { store.buffer(envelope(1, event)); store.flush(); };
    send({ type: "model_attempt_started", session_id: "session-1", run_id: "run-1", attempt: 2 });
    expect(store.runForSession("session-1")?.model_request).toEqual({ type: "requesting", attempt: 2 });
    send({ type: "model_stream_established", session_id: "session-1", run_id: "run-1", attempt: 2 });
    expect(store.runForSession("session-1")?.model_request).toBeNull();
    const retry: RuntimeEvent = { type: "model_retry_scheduled", session_id: "session-1", run_id: "run-1", next_attempt: 2, delay_ms: 2000 };
    send(retry);
    send({ type: "run_cancelling", session_id: "session-1", run_id: "run-1" });
    send(retry);
    expect(store.runForSession("session-1")?.model_request).toBeNull();
    send({ type: "run_finished", session_id: "session-1", run_id: "run-1", status: "cancelled", error: null });
    send(retry);
    expect(store.runForSession("session-1")?.model_request).toBeNull();
    send({ ...retry, session_id: "session-2", run_id: "run-2" });
    expect(store.runForSession("session-2")?.model_request?.type).toBe("retry_waiting");
    store.dispose();
  });
});


describe("reasoning source and reliable execution time", () => {
  afterEach(() => vi.unstubAllGlobals());

  function store() {
    vi.stubGlobal("requestAnimationFrame", vi.fn(() => 1));
    vi.stubGlobal("cancelAnimationFrame", vi.fn());
    return new LiveExecutionStore();
  }

  it("restores exact reasoning identities and sources before appending later deltas", () => {
    const live = store();
    const view = activeSessionView(1);
    view.active_run!.created_at_ms = 100;
    view.active_run!.finished_at_ms = null;
    view.active_run!.reasoning_parts = [
      { part_id: "summary", source: "summary", text: "short" },
      { part_id: "raw", text: "long" },
    ];
    live.reconcileSession(view, 200);
    live.buffer(envelope(2, { type: "reasoning_delta", session_id: "session-1", run_id: "run-1", step: 1, part_id: "summary", source: "summary", delta: " summary" }));
    live.flush();
    expect(live.runForSession("session-1")?.steps[0]?.segments).toEqual([
      { type: "reasoning", part_id: "summary", source: "summary", text: "short summary" },
      { type: "reasoning", part_id: "raw", source: "raw", text: "long" },
    ]);
    expect(live.runForSession("session-1")?.created_at_ms).toBe(100);
  });

  it("seals snapshot reasoning deltas without swallowing later or other-owner events", () => {
    const live = store();
    live.buffer(envelope(1, { type: "run_started", session_id: "session-1", run_id: "run-1" }));
    live.flush();
    const view = activeSessionView(1);
    view.active_run!.reasoning_parts = [{ part_id: "p", source: "summary", text: "already observed" }];
    live.reconcileSession(view, 100, 10);
    live.buffer(envelope(5, { type: "reasoning_delta", session_id: "session-1", run_id: "run-1", step: 1, part_id: "p", source: "summary", delta: "already observed" }));
    live.buffer(envelope(6, { type: "reasoning_delta", session_id: "other", run_id: "other-run", step: 1, part_id: "p", delta: "other owner" }));
    live.buffer(envelope(11, { type: "reasoning_delta", session_id: "session-1", run_id: "run-1", step: 1, part_id: "p", source: "summary", delta: " later" }));
    live.flush();
    expect(live.runForSession("session-1")?.steps[0]?.segments).toEqual([
      { type: "reasoning", part_id: "p", source: "summary", text: "already observed later" },
    ]);
    expect(live.runForSession("other")?.steps[0]?.segments[0]).toMatchObject({ text: "other owner" });
    // A late snapshot cannot erase a newer live suffix.
    live.reconcileSession(view, 100, 10);
    expect(live.runForSession("session-1")?.steps[0]?.segments[0]).toMatchObject({ text: "already observed later" });
  });

  it("captures the clock before the frame delay and clears it on owner reset", () => {
    const live = store();
    const now = vi.spyOn(performance, "now").mockReturnValue(50);
    live.buffer({ ...envelope(1, { type: "run_accepted", session_id: "s", run_id: "r", created_at_ms: 100 }), emitted_at_ms: 1000 });
    expect(live.clock_anchor).toBeNull();
    now.mockReturnValue(500);
    live.flush();
    expect(live.clock_anchor).toEqual({ host_at_ms: 1000, received_at_ms: 50 });
    expect(executionElapsedMs(live.runForSession("s")!, live.clock_anchor, 500)).toBeNull();
    live.buffer({ ...envelope(2, { type: "run_started", session_id: "s", run_id: "r", started_at_ms: 100 }), emitted_at_ms: 1000 });
    live.flush();
    expect(executionElapsedMs(live.runForSession("s")!, live.clock_anchor, 500)).toBe(900);
    live.clear();
    expect(live.clock_anchor).toBeNull();
    now.mockRestore();
  });

  it("never invents an acceptance time from late events and reconciles reliable terminal time", () => {
    const live = store();
    live.buffer(envelope(999, { type: "text_delta", session_id: "session-1", run_id: "run-1", step: 1, part_id: "p", delta: "late" }));
    live.flush();
    expect(live.runForSession("session-1")?.created_at_ms).toBeNull();
    const view = activeSessionView(1);
    view.active_run!.created_at_ms = 100;
    view.active_run!.started_at_ms = 100;
    view.active_run!.finished_at_ms = null;
    live.reconcileSession(view, 1000);
    live.buffer(envelope(2000, { type: "run_finished", session_id: "session-1", run_id: "run-1", status: "failed", error: null, finished_at_ms: 1500 }));
    live.flush();
    const run = live.runForSession("session-1")!;
    expect(executionElapsedMs(run, live.clock_anchor, 9999999)).toBe(1400);
    live.invalidateForGap();
    expect(live.clock_anchor).toBeNull();
  });

  it("keeps child source and child times separate from the parent", () => {
    const live = store();
    live.buffer(envelope(99, { type: "child_task_event", session_id: "session-1", parent_run_id: "parent", child_task_id: "child", event: { type: "reasoning_delta", step: 1, part_id: "p", source: "summary", delta: "child summary" } }));
    live.flush();
    expect(live.runForChildTask("session-1", "child")?.created_at_ms).toBeNull();
    expect(live.runForChildTask("session-1", "child")?.steps[0]?.segments[0]).toMatchObject({ source: "summary" });
    live.buffer(envelope(101, { type: "child_task_event", session_id: "session-1", parent_run_id: "parent", child_task_id: "child", event: { type: "finished", status: "cancelled", error: null, finished_at_ms: 100 } }));
    live.flush();
    expect(live.runForChildTask("session-1", "child")?.finished_at_ms).toBe(100);
    expect(live.runForSession("session-1")).toBeNull();
  });

  it("uses monotonic time against a Host anchor for active waits and freezes all terminal outcomes", () => {
    const run = { started_at_ms: 1000, finished_at_ms: null, status: "running" as const };
    const anchor = hostClockAnchor(6000, 30)!;
    expect(executionElapsedMs(run, anchor, 530)).toBe(5500);
    for (const status of ["completed", "failed", "cancelled", "interrupted", "compaction_required"] as const) {
      expect(executionElapsedMs({ ...run, status, finished_at_ms: 8000 }, anchor, 999999)).toBe(7000);
      expect(executionElapsedMs({ ...run, status }, anchor, 999999)).toBeNull();
    }
    for (const started_at_ms of [null, NaN, Infinity, -1, Number.MAX_SAFE_INTEGER + 1]) {
      expect(executionElapsedMs({ ...run, started_at_ms }, anchor, 530)).toBeNull();
    }
    expect(executionElapsedMs({ ...run, status: "completed", finished_at_ms: 999 }, anchor, 530)).toBeNull();
    expect(executionElapsedMs(run, null, 530)).toBeNull();
  });

  it("excludes a long queue wait and gives a retry its own reliable start", () => {
    const live = store();
    vi.spyOn(performance, "now").mockReturnValue(50);
    live.buffer({ ...envelope(1, { type: "run_accepted", session_id: "s", run_id: "first", created_at_ms: 100 }), emitted_at_ms: 1_000_000 });
    live.flush();
    expect(executionElapsedMs(live.runForSession("s")!, live.clock_anchor, 50)).toBeNull();
    live.buffer({ ...envelope(2, { type: "run_started", session_id: "s", run_id: "first", started_at_ms: 1_100_000 }), emitted_at_ms: 1_100_050 });
    live.buffer({ ...envelope(3, { type: "model_attempt_started", session_id: "s", run_id: "first", attempt: 2 }), emitted_at_ms: 1_100_050 });
    live.flush();
    expect(executionElapsedMs(live.runForSession("s")!, live.clock_anchor, 50)).toBe(50);
    live.buffer(envelope(4, { type: "run_finished", session_id: "s", run_id: "first", status: "failed", error: null, finished_at_ms: 1_100_300 }));
    live.buffer(envelope(5, { type: "run_accepted", session_id: "s", run_id: "retry", created_at_ms: 1_200_000 }));
    live.flush();
    expect(live.runForSession("s")?.started_at_ms).toBeNull();
    live.buffer({ ...envelope(6, { type: "run_started", session_id: "s", run_id: "retry", started_at_ms: 1_300_000 }), emitted_at_ms: 1_300_000 });
    live.flush();
    expect(executionElapsedMs(live.runForSession("s")!, live.clock_anchor, 50)).toBe(0);
  });

  it("uses a child's reliable start independently of its parent and accepts old events without inventing one", () => {
    const live = store();
    live.buffer(envelope(1, { type: "run_started", session_id: "s", run_id: "parent", started_at_ms: 100 }));
    live.buffer(envelope(2, { type: "child_task_event", session_id: "s", parent_run_id: "parent", child_task_id: "old", event: { type: "started" } }));
    live.buffer(envelope(3, { type: "child_task_event", session_id: "s", parent_run_id: "parent", child_task_id: "child", event: { type: "started", started_at_ms: 10_000 } }));
    live.buffer(envelope(4, { type: "child_task_event", session_id: "s", parent_run_id: "parent", child_task_id: "child", event: { type: "finished", status: "completed", error: null, finished_at_ms: 10_400 } }));
    live.flush();
    expect(live.runForChildTask("s", "old")?.started_at_ms).toBeNull();
    expect(executionElapsedMs(live.runForChildTask("s", "child")!, null, 0)).toBe(400);
    expect(executionElapsedMs({ started_at_ms: null, finished_at_ms: 50_000, status: "cancelled" }, null, 0)).toBeNull();
  });
});


it("discards interrupted text and reasoning while retaining settled tools", () => {
  const tool = { type: "tool_group" as const, group_id: "group", tools: [] };
  const steps = restartStep([{ step: 2, segments: [
    { type: "text", part_id: "old", text: "partial" },
    { type: "reasoning", part_id: "reason", text: "partial", source: "raw" },
    tool,
  ] }], 2);
  expect(steps).toEqual([{ step: 2, segments: [tool] }]);
});

// 队列接纳不能替代当前轮次，也不能单独生成正文占位。
describe("queued input visibility", () => {
  it("keeps running output selected when another input is accepted", () => {
    const store = new LiveExecutionStore();
    store.runs.set("s:running", { ...emptyRun("s", "running", 1), status: "running", started_at_ms: 2 });
    store.runs.set("s:queued", emptyRun("s", "queued", 3));
    expect(store.runForSession("s")?.run_id).toBe("running");
    store.runs.delete("s:running");
    expect(store.runForSession("s")?.status).toBe("accepted");
  });
});
