import { action, makeObservable, observable } from "mobx";
import type {
  ChildTaskId,
  ChildTaskSnapshot,
  ChildTaskStatus,
  ChildTaskViewSnapshot,
  RunSnapshot,
  RunStatus,
  RuntimeEventEnvelope,
  SessionId,
  SessionViewSnapshot,
  ToolCallId,
} from "../generated/assistant-protocol";
import {
  appendTextDelta,
  appendTool,
  emptyRun,
  ensureStep,
  reconcileTools,
  removeCommittedTools,
  runKey,
  updateCurrentStep,
  updateRunToolOutput,
  updateRunTools,
} from "./liveExecutionProjection";
import type { LiveExecutionSegment, LiveRunProjection } from "./liveExecutionProjection";

export type {
  LiveExecutionSegment,
  LiveExecutionStep,
  LiveReasoningSegment,
  LiveRunProjection,
  LiveTextSegment,
  LiveToolGroupSegment,
  LiveToolSnapshot,
} from "./liveExecutionProjection";

export class LiveExecutionStore {
  readonly runs = observable.map<string, LiveRunProjection>(undefined, { deep: false });
  readonly child_runs = observable.map<ChildTaskId, LiveRunProjection>(undefined, { deep: false });
  readonly child_tasks = observable.map<ChildTaskId, ChildTaskSnapshot>(undefined, { deep: false });
  #pending: RuntimeEventEnvelope[] = [];
  #animation_frame: number | null = null;

  constructor() {
    makeObservable(this, {
      runs: observable,
      child_runs: observable,
      child_tasks: observable,
      flush: action,
      reconcileSession: action,
      reconcileChildTask: action,
      clear: action,
    });
  }

  buffer(envelope: RuntimeEventEnvelope): void {
    if (!isLiveExecutionEvent(envelope)) {
      return;
    }
    this.#pending.push(envelope);
    if (this.#animation_frame === null) {
      this.#animation_frame = requestAnimationFrame(() => this.flush());
    }
  }

  flush(): void {
    if (this.#animation_frame !== null) {
      cancelAnimationFrame(this.#animation_frame);
    }
    for (const envelope of this.#pending) {
      this.#applyEvent(envelope);
    }
    this.#pending = [];
    this.#animation_frame = null;
  }

  reconcileSession(view: SessionViewSnapshot): void {
    // Apply events accepted before this snapshot first, so a delayed animation
    // frame cannot recreate a terminal live projection after reconciliation.
    if (this.#pending.length > 0) {
      this.flush();
    }
    const committed_steps = new Map<string, number>();
    const committed_tool_calls = new Map<string, Set<ToolCallId>>();
    for (const item of view.child_tasks ?? []) {
      this.child_tasks.set(item.task.child_task_id, item.task);
    }
    for (const item of view.conversation.items) {
      if (item.type !== "assistant" || !item.run_id) {
        continue;
      }
      committed_steps.set(item.run_id, (committed_steps.get(item.run_id) ?? 0) + 1);
      const calls = committed_tool_calls.get(item.run_id) ?? new Set<ToolCallId>();
      for (const segment of item.segments) {
        if (segment.type === "tool_group") {
          segment.tools.forEach((tool) => calls.add(tool.call_id));
        }
      }
      committed_tool_calls.set(item.run_id, calls);
    }
    for (const [key, run] of this.runs) {
      const is_current_session = run.session_id === view.session.session_id;
      const is_authoritatively_active = view.active_run?.run_id === run.run_id;
      if (is_current_session && (!view.active_run || (run.status !== "accepted" && !is_authoritatively_active))) {
        this.runs.delete(key);
        continue;
      }
      if (is_current_session && is_authoritatively_active) {
        const committed_step_count = committed_steps.get(run.run_id) ?? 0;
        const committed_calls = committed_tool_calls.get(run.run_id) ?? new Set<ToolCallId>();
        this.runs.set(key, {
          ...run,
          steps: run.steps
            .filter((item) => item.step > committed_step_count)
            .map((item) => ({ ...item, segments: removeCommittedTools(item.segments, committed_calls) }))
            .filter((item) => item.segments.length > 0),
        });
      }
    }
    if (view.active_run) {
      this.#reconcileRunSnapshot(
        view.active_run,
        committed_steps.get(view.active_run.run_id) ?? 0,
        committed_tool_calls.get(view.active_run.run_id) ?? new Set<ToolCallId>(),
      );
    }
  }

  runForSession(session_id: SessionId): LiveRunProjection | null {
    const candidates = [...this.runs.values()].filter((run) => run.session_id === session_id);
    return candidates.at(-1) ?? null;
  }

  runForChildTask(child_task_id: ChildTaskId): LiveRunProjection | null {
    return this.child_runs.get(child_task_id) ?? null;
  }

  childTasksForSession(session_id: SessionId): readonly ChildTaskSnapshot[] {
    return [...this.child_tasks.values()]
      .filter((task) => task.session_id === session_id)
      .sort((left, right) => left.created_at_ms - right.created_at_ms);
  }

  reconcileChildTask(view: ChildTaskViewSnapshot): void {
    if (this.#pending.length > 0) {
      this.flush();
    }
    const child_task_id = view.task.task.child_task_id;
    this.child_tasks.set(child_task_id, view.task.task);
    const committed_steps = view.conversation.items.filter((item) => item.type === "assistant").length;
    const committed_calls = new Set<ToolCallId>();
    for (const item of view.conversation.items) {
      if (item.type !== "assistant") {
        continue;
      }
      for (const segment of item.segments) {
        if (segment.type === "tool_group") {
          segment.tools.forEach((tool) => committed_calls.add(tool.call_id));
        }
      }
    }
    const status = childRunStatus(view.task.task.status);
    const current = this.child_runs.get(child_task_id);
    if (view.task.task.status === "completed" || view.task.task.status === "failed" || view.task.task.status === "cancelled" || view.task.task.status === "interrupted") {
      this.child_runs.delete(child_task_id);
      return;
    }
    const base = current ?? emptyRun(
      view.task.task.session_id,
      `child:${child_task_id}`,
      view.task.task.created_at_ms,
    );
    this.child_runs.set(child_task_id, {
      ...base,
      status,
      active_step: base.active_step || committed_steps + 1,
      steps: base.steps
        .filter((step) => step.step > committed_steps)
        .map((step) => ({ ...step, segments: removeCommittedTools(step.segments, committed_calls) }))
        .filter((step) => step.segments.length > 0),
    });
  }

  clear(): void {
    if (this.#animation_frame !== null) {
      cancelAnimationFrame(this.#animation_frame);
    }
    this.#animation_frame = null;
    this.#pending = [];
    this.runs.clear();
    this.child_runs.clear();
    this.child_tasks.clear();
  }

  dispose(): void {
    this.clear();
  }

  #applyEvent(envelope: RuntimeEventEnvelope): void {
    const event = envelope.event;
    if (event.type === "child_task_event") {
      const child_event = event.event;
      const current = this.child_runs.get(event.child_task_id)
        ?? emptyRun(event.session_id, `child:${event.child_task_id}`, envelope.emitted_at_ms);
      let next = current;
      switch (child_event.type) {
        case "created":
          this.child_tasks.set(event.child_task_id, child_event.task);
          next = { ...current, status: childRunStatus(child_event.task.status) };
          break;
        case "started":
          this.#updateChildTask(event.child_task_id, {
            status: "running",
            started_at_ms: envelope.emitted_at_ms,
          });
          next = { ...current, status: "running" };
          break;
        case "step_started":
          next = { ...current, active_step: child_event.step, steps: ensureStep(current.steps, child_event.step) };
          break;
        case "text_delta":
        case "reasoning_delta":
          next = updateCurrentStep(current, (segments) => appendTextDelta(
            segments,
            child_event.type === "text_delta" ? "text" : "reasoning",
            child_event.part_id,
            child_event.delta,
          ));
          break;
        case "tool_proposed":
          next = updateCurrentStep(current, (segments) => appendTool(segments, {
            call_id: child_event.call_id,
            tool_name: child_event.tool_name,
            status: "proposed",
            stdout: "",
            stderr: "",
          }));
          break;
        case "tool_started":
          next = updateRunTools(current, child_event.call_id, { status: "running" });
          break;
        case "tool_output":
          next = updateRunToolOutput(current, child_event.call_id, child_event.channel, child_event.chunk);
          break;
        case "tool_completed":
          next = updateRunTools(current, child_event.call_id, { status: child_event.status });
          break;
        case "usage_updated":
          next = { ...current, usage: child_event.usage };
          break;
        case "finished":
          this.#updateChildTask(event.child_task_id, {
            status: child_event.status,
            error: child_event.error,
            finished_at_ms: envelope.emitted_at_ms,
          });
          next = {
            ...current,
            status: childRunStatus(child_event.status),
            error_message: child_event.error?.message ?? current.error_message,
          };
          break;
      }
      this.child_runs.set(event.child_task_id, next);
      return;
    }
    if (!("session_id" in event) || !("run_id" in event)) {
      return;
    }
    const key = runKey(event.session_id, event.run_id);
    const current = this.runs.get(key) ?? emptyRun(event.session_id, event.run_id, envelope.emitted_at_ms);
    let next = current;
    switch (event.type) {
      case "run_accepted":
        next = { ...current, status: "accepted" };
        break;
      case "run_started":
        next = { ...current, status: "running" };
        break;
      case "run_cancelling":
        next = { ...current, status: "cancelling" };
        break;
      case "step_started":
        next = {
          ...current,
          active_step: event.step,
          steps: ensureStep(current.steps, event.step),
        };
        break;
      case "text_delta":
      case "reasoning_delta":
        next = updateCurrentStep(current, (segments) =>
          appendTextDelta(
            segments,
            event.type === "text_delta" ? "text" : "reasoning",
            event.part_id,
            event.delta,
          ),
        );
        break;
      case "tool_proposed":
        next = updateCurrentStep(current, (segments) =>
          appendTool(segments, {
            call_id: event.call_id,
            tool_name: event.tool_name,
            status: "proposed",
            stdout: "",
            stderr: "",
          }),
        );
        break;
      case "tool_started":
        next = updateRunTools(current, event.call_id, { status: "running" });
        break;
      case "tool_output":
        next = updateRunToolOutput(current, event.call_id, event.channel, event.chunk);
        break;
      case "tool_completed":
        next = updateRunTools(current, event.call_id, { status: event.status });
        break;
      case "usage_updated":
        next = { ...current, usage: event.usage };
        break;
      case "run_finished":
        next = {
          ...current,
          status: event.status,
          error_message: event.error?.message ?? current.error_message,
        };
        break;
      default:
        return;
    }
    this.runs.set(key, next);
  }

  #updateChildTask(
    child_task_id: ChildTaskId,
    patch: Partial<ChildTaskSnapshot>,
  ): void {
    const current = this.child_tasks.get(child_task_id);
    if (current) {
      this.child_tasks.set(child_task_id, { ...current, ...patch });
    }
  }

  #reconcileRunSnapshot(
    snapshot: RunSnapshot,
    committed_step_count = 0,
    committed_call_ids: ReadonlySet<ToolCallId> = new Set<ToolCallId>(),
  ): void {
    const key = runKey(snapshot.session_id, snapshot.run_id);
    const current = this.runs.get(key);
    const snapshot_tools = snapshot.tools.filter((tool) => !committed_call_ids.has(tool.call_id));
    if (current) {
      const active_step = current.active_step || committed_step_count + 1;
      const steps = ensureStep(current.steps, active_step).map((step) =>
        step.step === active_step
          ? { ...step, segments: reconcileTools(step.segments, snapshot_tools) }
          : step,
      );
      this.runs.set(key, {
        ...current,
        status: snapshot.status,
        error_message: snapshot.error?.message ?? current.error_message,
        active_step,
        steps,
      });
      return;
    }
    const segments: LiveExecutionSegment[] = [];
    if (snapshot.reasoning) {
      segments.push({ type: "reasoning", part_id: `snapshot-reasoning:${snapshot.run_id}`, text: snapshot.reasoning });
    }
    if (snapshot.text) {
      segments.push({ type: "text", part_id: `snapshot-text:${snapshot.run_id}`, text: snapshot.text });
    }
    if (snapshot_tools.length > 0) {
      segments.push({
        type: "tool_group",
        group_id: `snapshot-tools:${snapshot.run_id}`,
        tools: snapshot_tools,
      });
    }
    const active_step = committed_step_count + 1;
    this.runs.set(key, {
      session_id: snapshot.session_id,
      run_id: snapshot.run_id,
      created_at_ms: Date.now(),
      status: snapshot.status,
      active_step,
      steps: segments.length > 0 ? [{ step: active_step, segments }] : [],
      usage: null,
      error_message: snapshot.error?.message ?? null,
    });
  }
}

function isLiveExecutionEvent(envelope: RuntimeEventEnvelope): boolean {
  return [
    "child_task_event",
    "run_accepted",
    "run_started",
    "run_cancelling",
    "step_started",
    "text_delta",
    "reasoning_delta",
    "tool_proposed",
    "tool_started",
    "tool_output",
    "tool_completed",
    "usage_updated",
    "run_finished",
  ].includes(envelope.event.type);
}

function childRunStatus(status: ChildTaskStatus): RunStatus {
  switch (status) {
    case "accepted": return "accepted";
    case "running": return "running";
    case "completed": return "completed";
    case "failed": return "failed";
    case "cancelled": return "cancelled";
    case "interrupted": return "interrupted";
  }
}
