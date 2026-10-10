import { action, makeObservable, observable } from "mobx";
import type {
  ChildTaskId,
  ChildTaskSnapshot,
  ChildTaskStatus,
  ChildTaskViewSnapshot,
  ConversationOwner,
  RunSnapshot,
  RunStatus,
  RuntimeEventEnvelope,
  SessionId,
  SessionViewSnapshot,
} from "@ez-assistant/protocol";
import {
  hostClockAnchor,
  appendTextDelta,
  appendTool,
  emptyRun,
  restartStep,
  reconcileTools,
  runKey,
  updateStep,
  updateRunToolOutput,
  updateRunTools,
} from "./liveExecutionProjection";
import type { HostClockAnchor, LiveExecutionSegment, LiveRunProjection } from "./liveExecutionProjection";

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
  clock_anchor: HostClockAnchor | null = null;
  #pending_clock_anchor: HostClockAnchor | null = null;
  // 只封住本 Run 已纳入快照的 reasoning，不能用页面水位吞掉其他 owner 的事件。
  #reasoning_waterlines = new Map<string, { snapshot: number; latest: number }>();
  #pending: RuntimeEventEnvelope[] = [];
  #animation_frame: number | null = null;
  #sealed_run_steps = new Map<string, Set<number>>();
  #sealed_child_steps = new Map<ChildTaskId, Set<number>>();
  #session_generations = new Map<SessionId, number>();
  #child_generations = new Map<ChildTaskId, number>();

  constructor() {
    makeObservable(this, {
      clock_anchor: observable,
      runs: observable,
      child_runs: observable,
      child_tasks: observable,
      flush: action,
      reconcileSession: action,
      reconcileChildTask: action,
      invalidateForGap: action,
      clear: action,
    });
  }

  buffer(envelope: RuntimeEventEnvelope): void {
    // 在进入帧缓冲前取接收基准，后台页延迟 flush 不会重置业务起点。
    if (!isLiveExecutionEvent(envelope)) {
      return;
    }
    this.#pending_clock_anchor = hostClockAnchor(envelope.emitted_at_ms, performance.now()) ?? this.#pending_clock_anchor;
    this.#pending.push(envelope);
    if (this.#animation_frame === null) {
      this.#animation_frame = requestAnimationFrame(() => this.flush());
    }
  }

  flush(): void {
    // 显示时钟与正文一起按帧发布，避免每个 delta 触发一次独立 UI 更新。
    this.clock_anchor = this.#pending_clock_anchor ?? this.clock_anchor;
    this.#pending_clock_anchor = null;
    if (this.#animation_frame !== null) {
      cancelAnimationFrame(this.#animation_frame);
    }
    for (const envelope of this.#pending) {
      this.#applyEvent(envelope);
    }
    this.#pending = [];
    this.#animation_frame = null;
  }

  reconcileSession(view: SessionViewSnapshot, observed_at_ms?: number | null, observed_sequence?: number): void {
    const received_at_ms = performance.now();
    // Apply events accepted before this snapshot first, so a delayed animation
    // frame cannot recreate a terminal live projection after reconciliation.
    if (this.#pending.length > 0) {
      this.flush();
    }
    this.clock_anchor = hostClockAnchor(observed_at_ms, received_at_ms) ?? this.clock_anchor;
    const session_id = view.session.session_id;
    if (view.conversation.owner?.type === "main_session"
      && view.conversation.owner.session_id === session_id) {
      const previous_generation = this.#session_generations.get(session_id);
      if (previous_generation !== undefined && previous_generation !== view.conversation.generation) {
        this.#invalidateMainOwner(session_id);
      }
      this.#session_generations.set(session_id, view.conversation.generation);
    }
    const authoritative_child_task_ids = new Set(
      (view.child_tasks ?? []).map((item) => item.task.child_task_id),
    );
    for (const [child_task_id, task] of this.child_tasks) {
      if (task.session_id === session_id && !authoritative_child_task_ids.has(child_task_id)) {
        this.child_tasks.delete(child_task_id);
        this.child_runs.delete(child_task_id);
        this.#sealed_child_steps.delete(child_task_id);
        this.#child_generations.delete(child_task_id);
      }
    }
    for (const [child_task_id, run] of this.child_runs) {
      if (run.session_id === session_id && !authoritative_child_task_ids.has(child_task_id)) {
        this.child_runs.delete(child_task_id);
        this.#sealed_child_steps.delete(child_task_id);
        this.#child_generations.delete(child_task_id);
      }
    }
    const latest_run = view.runs?.at(-1);
    const committed_steps = new Map<string, Set<number>>();
    for (const item of view.child_tasks ?? []) {
      this.child_tasks.set(item.task.child_task_id, item.task);
      const child_run = this.child_runs.get(item.task.child_task_id);
      if (child_run) {
        this.child_runs.set(item.task.child_task_id, {
          ...child_run,
          created_at_ms: item.task.created_at_ms,
          finished_at_ms: item.task.finished_at_ms,
        });
      }
    }
    for (const item of view.conversation.items) {
      if (item.type !== "assistant" || !item.run_id) {
        continue;
      }
      if (item.step !== undefined) {
        const steps = committed_steps.get(item.run_id) ?? new Set<number>();
        steps.add(item.step);
        committed_steps.set(item.run_id, steps);
      }
    }
    for (const [key, run] of this.runs) {
      const is_current_session = run.session_id === view.session.session_id;
      const is_authoritatively_active = view.active_run?.run_id === run.run_id;
      const retain_terminal_error = !view.active_run
        && (!latest_run || latest_run.run_id === run.run_id)
        && run.error_message !== null
        && (run.status === "failed" || run.status === "interrupted" || run.status === "compaction_required");
      if (is_current_session && (!view.active_run || (run.status !== "accepted" && !is_authoritatively_active))
        && !retain_terminal_error) {
        this.runs.delete(key);
        this.#reasoning_waterlines.delete(key);
        this.#sealed_run_steps.delete(key);
        continue;
      }
      if (is_current_session && is_authoritatively_active) {
        const committed = committed_steps.get(run.run_id) ?? new Set<number>();
        this.runs.set(key, {
          ...run,
          steps: run.steps.filter((item) => !committed.has(item.step)),
        });
      }
    }
    if (view.active_run) {
      const baseline = committed_steps.get(view.active_run.run_id) ?? new Set<number>();
      for (let step = 1; step < (view.active_run.active_step ?? 0); step += 1) {
        baseline.add(step);
      }
      this.#sealed_run_steps.set(runKey(session_id, view.active_run.run_id), baseline);
      this.#reconcileRunSnapshot(
        view.active_run,
        baseline,
        observed_sequence,
      );
    } else if (latest_run?.status === "failed" && latest_run.error) {
      // 重连后失败事实来自既有持久 Run，不能只保留收到过终态 SSE 的页面。
      this.#reconcileRunSnapshot(latest_run, new Set(), observed_sequence);
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

  reconcileChildTask(view: ChildTaskViewSnapshot, observed_at_ms?: number | null): void {
    const received_at_ms = performance.now();
    if (this.#pending.length > 0) {
      this.flush();
    }
    this.clock_anchor = hostClockAnchor(observed_at_ms, received_at_ms) ?? this.clock_anchor;
    const child_task_id = view.task.task.child_task_id;
    const previous_generation = this.#child_generations.get(child_task_id);
    if (previous_generation !== undefined && previous_generation !== view.conversation.generation) {
      this.#invalidateChildOwner(child_task_id);
    }
    this.#child_generations.set(child_task_id, view.conversation.generation);
    this.child_tasks.set(child_task_id, view.task.task);
    const status = childRunStatus(view.task.task.status);
    const current = this.child_runs.get(child_task_id);
    if (view.task.task.status === "completed" || view.task.task.status === "failed" || view.task.task.status === "cancelled" || view.task.task.status === "interrupted") {
      this.child_runs.delete(child_task_id);
      this.#sealed_child_steps.delete(child_task_id);
      return;
    }
    const base = current ?? emptyRun(
      view.task.task.session_id,
      `child:${child_task_id}`,
      view.task.task.created_at_ms,
    );
    this.child_runs.set(child_task_id, {
      ...base,
      created_at_ms: view.task.task.created_at_ms,
      finished_at_ms: view.task.task.finished_at_ms,
      status,
      active_step: base.active_step,
      steps: base.steps,
    });
  }

  clear(): void {
    if (this.#animation_frame !== null) {
      cancelAnimationFrame(this.#animation_frame);
    }
    this.#animation_frame = null;
    this.#pending = [];
    this.clock_anchor = null;
    this.#pending_clock_anchor = null;
    this.runs.clear();
    this.#reasoning_waterlines.clear();
    this.child_runs.clear();
    this.child_tasks.clear();
    this.#sealed_run_steps.clear();
    this.#sealed_child_steps.clear();
    this.#session_generations.clear();
    this.#child_generations.clear();
  }

  invalidateForGap(): void {
    if (this.#animation_frame !== null) {
      cancelAnimationFrame(this.#animation_frame);
    }
    this.#animation_frame = null;
    this.#pending = [];
    this.clock_anchor = null;
    this.#pending_clock_anchor = null;
    this.runs.clear();
    this.#reasoning_waterlines.clear();
    this.child_runs.clear();
    this.#sealed_run_steps.clear();
    this.#sealed_child_steps.clear();
    this.#session_generations.clear();
    this.#child_generations.clear();
  }

  dispose(): void {
    this.clear();
  }

  #applyEvent(envelope: RuntimeEventEnvelope): void {
    const event = envelope.event;
    if (event.type === "child_task_event") {
      const child_event = event.event;
      if ("step" in child_event
        && this.#sealed_child_steps.get(event.child_task_id)?.has(child_event.step)) {
        return;
      }
      const current = this.child_runs.get(event.child_task_id)
        ?? emptyRun(event.session_id, `child:${event.child_task_id}`, this.child_tasks.get(event.child_task_id)?.created_at_ms ?? null);
      if ("step" in child_event && !isNonterminalRun(current.status)) {
        return;
      }
      let next = current;
      switch (child_event.type) {
        case "created":
          this.child_tasks.set(event.child_task_id, child_event.task);
          next = { ...current, status: childRunStatus(child_event.task.status), created_at_ms: child_event.task.created_at_ms, finished_at_ms: child_event.task.finished_at_ms };
          break;
        case "started":
          this.#updateChildTask(event.child_task_id, {
            status: "running",
            // Started 不携带可靠时刻；等待 child 快照补齐。
            started_at_ms: this.child_tasks.get(event.child_task_id)?.started_at_ms ?? null,
          });
          next = { ...current, status: "running" };
          break;
        case "step_started":
          next = { ...current, active_step: child_event.step, steps: restartStep(current.steps, child_event.step) };
          break;
        case "text_delta":
        case "reasoning_delta":
          next = updateStep(current, child_event.step, (segments) => appendTextDelta(
            segments,
            child_event.type === "text_delta" ? "text" : "reasoning",
            child_event.part_id,
            child_event.delta,
            child_event.type === "reasoning_delta" ? child_event.source : undefined,
          ));
          break;
        case "tool_proposed":
          next = updateStep(current, child_event.step, (segments) => appendTool(segments, {
            call_id: child_event.call_id,
            tool_name: child_event.tool_name,
            input: child_event.input,
            status: "proposed",
            stdout: "",
            stderr: "",
          }));
          break;
        case "tool_started":
          next = updateRunTools(current, child_event.step, child_event.call_id, { status: "running" });
          break;
        case "tool_output":
          next = updateRunToolOutput(current, child_event.step, child_event.call_id, child_event.channel, child_event.chunk);
          break;
        case "tool_completed":
          next = updateRunTools(current, child_event.step, child_event.call_id, { status: child_event.status });
          break;
        case "usage_updated":
          next = { ...current, usage: child_event.usage };
          break;
        case "finished":
          this.#updateChildTask(event.child_task_id, {
            status: child_event.status,
            error: child_event.error,
            finished_at_ms: child_event.finished_at_ms ?? null,
          });
          next = {
            ...current,
            status: childRunStatus(child_event.status),
            finished_at_ms: child_event.finished_at_ms ?? null,
            error_message: child_event.error?.message ?? current.error_message,
          };
          this.#sealed_child_steps.delete(event.child_task_id);
          break;
      }
      this.child_runs.set(event.child_task_id, next);
      return;
    }
    if (event.type === "step_committed") {
      this.#sealStep(event.owner, event.step, event.generation);
      return;
    }
    if (!("session_id" in event) || !("run_id" in event)) {
      return;
    }
    const key = runKey(event.session_id, event.run_id);
    const current = this.runs.get(key) ?? emptyRun(event.session_id, event.run_id, null);
    if (event.type === "reasoning_delta") {
      const waterline = this.#reasoning_waterlines.get(key) ?? { snapshot: 0, latest: 0 };
      if (envelope.sequence <= waterline.snapshot) return;
      this.#reasoning_waterlines.set(key, { ...waterline, latest: envelope.sequence });
    }
    if ("step" in event) {
      if (!isNonterminalRun(current.status) || this.#sealed_run_steps.get(key)?.has(event.step)) {
        return;
      }
    }
    // attempt 事件不带 step；终态与取消同样必须阻止迟到事件恢复重试提示。
    if (event.type.startsWith("model_")
      && (!isNonterminalRun(current.status) || current.status === "cancelling")) {
      return;
    }
    let next = current;
    switch (event.type) {
      case "run_accepted":
        for (const [existing_key, existing_run] of this.runs) {
          if (existing_key !== key && existing_run.session_id === event.session_id
            && existing_run.error_message !== null) {
            this.runs.delete(existing_key);
            this.#reasoning_waterlines.delete(existing_key);
            this.#sealed_run_steps.delete(existing_key);
          }
        }
        next = { ...current, status: "accepted", created_at_ms: event.created_at_ms ?? current.created_at_ms };
        break;
      case "run_started":
        next = { ...current, status: "running" };
        break;
      case "run_cancelling":
        next = { ...current, status: "cancelling", model_request: null };
        break;
      case "model_attempt_started":
        next = { ...current, model_request: { type: "requesting", attempt: event.attempt }, model_failure_kind: null };
        break;
      case "model_retry_scheduled":
        next = { ...current, model_request: { type: "retry_waiting", next_attempt: event.next_attempt, delay_ms: event.delay_ms } };
        break;
      case "model_attempt_failed":
        next = {
          ...current,
          model_request: null,
          model_failure_kind: event.will_retry ? current.model_failure_kind : event.kind,
        };
        break;
      case "model_stream_established":
        next = { ...current, model_failure_kind: null, model_request: null };
        break;
      case "step_started":
        next = {
          ...current,
          active_step: event.step,
          steps: restartStep(current.steps, event.step),
        };
        break;
      case "text_delta":
      case "reasoning_delta":
        next = updateStep(current, event.step, (segments) =>
          appendTextDelta(
            segments,
            event.type === "text_delta" ? "text" : "reasoning",
            event.part_id,
            event.delta,
            event.type === "reasoning_delta" ? event.source : undefined,
          ),
        );
        break;
      case "tool_proposed":
        next = updateStep(current, event.step, (segments) =>
          appendTool(segments, {
            call_id: event.call_id,
            tool_name: event.tool_name,
            input: event.input,
            status: "proposed",
            stdout: "",
            stderr: "",
          }),
        );
        break;
      case "tool_started":
        next = updateRunTools(current, event.step, event.call_id, { status: "running" });
        break;
      case "tool_output":
        next = updateRunToolOutput(current, event.step, event.call_id, event.channel, event.chunk);
        break;
      case "tool_completed":
        next = updateRunTools(current, event.step, event.call_id, { status: event.status });
        break;
      case "usage_updated":
        next = { ...current, usage: event.usage };
        break;
      case "run_finished":
        next = {
          ...current,
          status: event.status,
          finished_at_ms: event.finished_at_ms ?? null,
          model_request: null,
          error_code: event.error?.code ?? null,
          error_message: event.error?.message ?? null,
          model_failure_kind: event.status === "failed" ? current.model_failure_kind : null,
        };
        this.#sealed_run_steps.delete(key);
        break;
      default:
        return;
    }
    this.runs.set(key, next);
  }

  #sealStep(owner: ConversationOwner, step: number, generation: number): void {
    if (owner.type === "main_session") {
      if (this.#session_generations.get(owner.session_id) !== generation) {
        this.#invalidateMainOwner(owner.session_id);
        return;
      }
      const candidates = [...this.runs.entries()].filter(([, run]) =>
        run.session_id === owner.session_id && isNonterminalRun(run.status));
      if (candidates.length !== 1) {
        this.#invalidateMainOwner(owner.session_id);
        return;
      }
      const [key, run] = candidates[0]!;
      const sealed = this.#sealed_run_steps.get(key) ?? new Set<number>();
      sealed.add(step);
      this.#sealed_run_steps.set(key, sealed);
      this.runs.set(key, { ...run, steps: run.steps.filter((item) => item.step !== step) });
      return;
    }

    if (this.#child_generations.get(owner.child_task_id) !== generation) {
      this.#invalidateChildOwner(owner.child_task_id);
      return;
    }
    const run = this.child_runs.get(owner.child_task_id);
    if (!run || run.session_id !== owner.session_id || !isNonterminalRun(run.status)) {
      this.#invalidateChildOwner(owner.child_task_id);
      return;
    }
    const sealed = this.#sealed_child_steps.get(owner.child_task_id) ?? new Set<number>();
    sealed.add(step);
    this.#sealed_child_steps.set(owner.child_task_id, sealed);
    this.child_runs.set(owner.child_task_id, {
      ...run,
      steps: run.steps.filter((item) => item.step !== step),
    });
  }

  #invalidateMainOwner(session_id: SessionId): void {
    for (const [key, run] of this.runs) {
      if (run.session_id === session_id) {
        this.runs.delete(key);
        this.#reasoning_waterlines.delete(key);
        this.#sealed_run_steps.delete(key);
      }
    }
    this.#session_generations.delete(session_id);
  }

  #invalidateChildOwner(child_task_id: ChildTaskId): void {
    this.child_runs.delete(child_task_id);
    this.#sealed_child_steps.delete(child_task_id);
    this.#child_generations.delete(child_task_id);
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
    committed_steps: ReadonlySet<number> = new Set<number>(),
    observed_sequence?: number,
  ): void {
    const key = runKey(snapshot.session_id, snapshot.run_id);
    const current = this.runs.get(key);
    const waterline = this.#reasoning_waterlines.get(key);
    const replace_reasoning = observed_sequence !== undefined
      && observed_sequence >= (waterline?.latest ?? 0);
    if (replace_reasoning) {
      this.#reasoning_waterlines.set(key, { snapshot: observed_sequence, latest: observed_sequence });
    }
    const active_step = snapshot.active_step ?? current?.active_step ?? 0;
    const snapshot_tools = snapshot.tools.filter((tool) =>
      tool.step === undefined || !committed_steps.has(tool.step));
    if (current) {
      let next: LiveRunProjection = {
        ...current,
        steps: current.steps.filter((step) => !committed_steps.has(step.step)),
      };
      if (replace_reasoning && active_step > 0 && !committed_steps.has(active_step)) {
        next = updateStep(next, active_step, (segments) => {
          const parts = snapshot.reasoning_parts.map((part) => ({ ...part, type: "reasoning" as const, source: part.source ?? "raw" as const }));
          const existing_ids = new Set(segments.filter((part) => part.type === "reasoning").map((part) => part.part_id));
          return [
            ...parts.filter((part) => !existing_ids.has(part.part_id)),
            ...segments.flatMap((segment): LiveExecutionSegment[] => segment.type === "reasoning"
              ? parts.filter((part) => part.part_id === segment.part_id)
              : [segment]),
          ];
        });
      }
      for (const tool of snapshot_tools) {
        const step = tool.step ?? active_step;
        if (step > 0) {
          next = updateStep(next, step, (segments) => reconcileTools(segments, [tool]));
        }
      }
      this.runs.set(key, {
        ...next,
        status: snapshot.status,
        created_at_ms: snapshot.created_at_ms,
        finished_at_ms: snapshot.finished_at_ms,
        model_request: snapshot.model_request ?? null,
        error_code: snapshot.error?.code ?? current.error_code,
        error_message: snapshot.error?.message ?? current.error_message,
        active_step,
      });
      return;
    }
    const segments: LiveExecutionSegment[] = [];
    for (const part of snapshot.reasoning_parts) {
      segments.push({ type: "reasoning", part_id: part.part_id, text: part.text, source: part.source ?? "raw" });
    }
    if (snapshot.text) {
      segments.push({ type: "text", part_id: `snapshot-text:${snapshot.run_id}`, text: snapshot.text });
    }
    const active_tools = snapshot_tools.filter((tool) => (tool.step ?? active_step) === active_step);
    if (active_tools.length > 0) {
      segments.push({
        type: "tool_group",
        group_id: `snapshot-tools:${snapshot.run_id}`,
        tools: active_tools,
      });
    }
    const extra_steps = snapshot_tools
      .filter((tool) => tool.step !== undefined && tool.step !== active_step)
      .reduce<LiveRunProjection>((run, tool) => updateStep(run, tool.step!, (items) => reconcileTools(items, [tool])), {
        ...emptyRun(snapshot.session_id, snapshot.run_id, snapshot.created_at_ms),
        status: snapshot.status,
        created_at_ms: snapshot.created_at_ms,
        finished_at_ms: snapshot.finished_at_ms,
        model_request: snapshot.model_request ?? null,
        active_step,
        steps: segments.length > 0 && active_step > 0 && !committed_steps.has(active_step) ? [{ step: active_step, segments }] : [],
      });
    this.runs.set(key, {
      ...extra_steps,
      status: snapshot.status,
      created_at_ms: snapshot.created_at_ms,
      finished_at_ms: snapshot.finished_at_ms,
      model_request: snapshot.model_request ?? null,
      active_step,
      error_code: snapshot.error?.code ?? null,
      error_message: snapshot.error?.message ?? null,
    });
  }
}

function isLiveExecutionEvent(envelope: RuntimeEventEnvelope): boolean {
  return [
    "child_task_event",
    "step_committed",
    "run_accepted",
    "run_started",
    "run_cancelling",
    "model_attempt_started",
    "model_retry_scheduled",
    "model_attempt_failed",
    "model_stream_established",
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

function isNonterminalRun(status: RunStatus): boolean {
  return status === "accepted" || status === "running" || status === "cancelling";
}
