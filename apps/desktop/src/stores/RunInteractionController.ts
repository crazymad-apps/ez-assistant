import { observable, runInAction } from "mobx";
import type {
  AgentVariant,
  ApprovalDecision,
  ApprovalId,
  AttachmentId,
  ChildTaskId,
  GoalId,
  InputDelivery,
  InputId,
  InterventionNotExecuted,
  McpServerKey,
  RunId,
  ReasoningEffortKey,
  QuotedTextSnapshot,
  SessionId,
  SessionCommand,
  SubmitInputMode,
  SubmitInputRequest,
  SubmitInputResult,
} from "@ez-assistant/protocol";
import { isCommandOutcomeUnknown, type RuntimeClient } from "../runtime-client/RuntimeClient";
import type { ConnectionStore } from "./ConnectionStore";
import type { RuntimeLifecycleCoordinator } from "./RuntimeLifecycleCoordinator";

type RunInteractionState = {
  composer_pending: boolean;
  interaction_error: string | null;
  pending_approval_id: ApprovalId | null;
  pending_queue_input_id: InputId | null;
};

type RunInteractionDependencies = Readonly<{
  connection: ConnectionStore;
  runtime: RuntimeLifecycleCoordinator;
  state: RunInteractionState;
  input_scope: () => string;
  supports_intervention: () => boolean;
  queue_notice: (session_id: SessionId, message: string) => void;
}>;

type InterventionTarget =
  | { type: "draft"; payload: SubmitInputRequest; on_accepted?: () => void }
  | { type: "queue"; input_id: InputId };

type InterventionAttempt = Readonly<{
  target: InterventionTarget;
  client: RuntimeClient;
  runtime: RuntimeLifecycleCoordinator;
  phase: "submitting" | "confirming" | "uncertain";
}>;

/** Owns input, queue, approval and active-run command workflows. */
export class RunInteractionController {
  readonly #interventions = observable.map<string, InterventionAttempt>(undefined, { deep: false });

  constructor(private readonly dependencies: RunInteractionDependencies) {}

  get intervention() {
    const attempt = this.#interventions.get(this.dependencies.input_scope());
    return attempt?.runtime === this.dependencies.runtime ? attempt : undefined;
  }

  async cancelChildTask(session_id: SessionId, child_task_id: ChildTaskId): Promise<void> {
    const { runtime, state } = this.dependencies;
    if (!runtime.client || state.composer_pending) {
      return;
    }
    state.composer_pending = true;
    state.interaction_error = null;
    try {
      await runtime.client.command({
        type: "cancel_child_task",
        payload: { session_id, child_task_id },
      });
      await runtime.loadSession(session_id);
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
    } finally {
      runInAction(() => {
        state.composer_pending = false;
      });
    }
  }

  async submitInput(
    session_id: SessionId,
    message: string,
    variant: AgentVariant,
    attachment_ids: readonly AttachmentId[] = [],
    mode: SubmitInputMode = "normal",
    skill_name: string | null = null,
    quotes: readonly QuotedTextSnapshot[] = [],
    mcp_server_key: McpServerKey | null = null,
    delivery?: InputDelivery,
    on_accepted?: () => void,
  ): Promise<boolean> {
    const { connection, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (
      !client
      || connection.state !== "connected"
      || state.composer_pending
      || this.intervention
      || (!message.trim() && attachment_ids.length === 0 && quotes.length === 0)
    ) {
      return false;
    }
    const payload: SubmitInputRequest = {
      session_id, message, variant, mode, delivery,
      attachment_ids: [...attachment_ids], quotes: [...quotes],
      skill_name: skill_name ?? undefined, mcp_server_key: mcp_server_key ?? undefined,
      idempotency_key: createIdempotencyKey(),
    };
    if (delivery === "intervention") {
      if (mode !== "normal" || state.pending_queue_input_id || !this.dependencies.supports_intervention()) return false;
      return this.#sendIntervention(session_id, { type: "draft", payload, on_accepted }, client, runtime);
    }
    state.composer_pending = true;
    state.interaction_error = null;
    try {
      await client.command({ type: "submit_input", payload });
      await runtime.loadSession(session_id);
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        state.composer_pending = false;
      });
    }
  }

  async interveneQueuedInput(session_id: SessionId, input_id: InputId): Promise<void> {
    const { runtime, connection, state } = this.dependencies;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || this.intervention || state.composer_pending || state.pending_queue_input_id
      || !this.dependencies.supports_intervention()) return;
    await this.#sendIntervention(session_id, { type: "queue", input_id }, client, runtime);
  }

  async confirmIntervention(session_id: SessionId, automatic: boolean): Promise<void> {
    const { connection, runtime } = this.dependencies;
    const attempt = this.intervention;
    const client = runtime.client;
    if (!client || connection.state !== "connected" || !this.dependencies.supports_intervention()
      || attempt?.phase !== "uncertain" || (automatic && attempt.client === client)) return;
    await this.#sendIntervention(session_id, attempt.target, client, runtime, true);
  }

  /** 直发核实沿用完整载荷和原键。队列项只有 InputId，回执丢失后只查可靠事实，避免再次打断。 */
  async #sendIntervention(session_id: SessionId, target: InterventionTarget, client: RuntimeClient,
    runtime: RuntimeLifecycleCoordinator, confirming = false): Promise<boolean> {
    const { state, input_scope, queue_notice } = this.dependencies;
    const scope = input_scope();
    const attempt: InterventionAttempt = { target, runtime, client, phase: confirming ? "confirming" : "submitting" };
    const current = () => this.dependencies.runtime === runtime && input_scope() === scope;
    runInAction(() => {
      this.#interventions.set(scope, attempt);
      state.interaction_error = null;
    });
    try {
      let result: SubmitInputResult;
      if (target.type === "draft") {
        result = (await client.command({ type: "submit_input", payload: target.payload })).payload;
      } else if (!confirming) {
        result = (await client.command({ type: "intervene_queued_input", payload: { session_id, input_id: target.input_id } })).payload;
      } else {
        const view = (await client.command({ type: "get_session_view", payload: { session_id } })).payload.snapshot.value;
        const run = view.runs.find(item => item.input_id === target.input_id) ?? null;
        const message = view.conversation.items.find(item => item.type === "user" && item.input_id === target.input_id);
        const queued = view.queue.items.some(item => item.type === "message" && item.payload.input_id === target.input_id);
        if (!queued && !run && !message) {
          if (current()) runInAction(() => queue_notice(session_id, "原队列项已不在队列，请查看会话历史。"));
          return false;
        }
        result = { input_id: target.input_id, run, generation: view.conversation.generation,
          user_message: message?.type === "user" ? message : undefined,
          not_executed: queued ? view.queue.state === "paused_by_user" ? "queue_paused" : "queued" : undefined };
      }
      runInAction(() => {
        if (target.type === "draft") target.on_accepted?.();
        if (client !== runtime.client || !current()) return;
        runtime.applyInputReceipt(session_id, result);
        if (result.not_executed) queue_notice(session_id, QUEUE_NOTICES[result.not_executed]);
      });
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        if ((confirming && target.type === "queue") || isCommandOutcomeUnknown(error)) {
          this.#interventions.set(scope, { ...attempt, phase: "uncertain" });
        } else if (current()) state.interaction_error = displayError(error);
      });
      return false;
    } finally {
      runInAction(() => {
        if (this.#interventions.get(scope) === attempt) this.#interventions.delete(scope);
      });
      if (current() && client === runtime.client) void runtime.loadSession(session_id);
    }
  }

  async prioritizeQueuedInput(session_id: SessionId, input_id: InputId, revision: number): Promise<void> {
    await this.#runQueueMutation(session_id, input_id, () => this.dependencies.runtime.client!.command({
      type: "prioritize_queued_input",
      payload: { session_id, input_id, expected_revision: revision },
    }));
  }

  async submitSessionCommand(session_id: SessionId, command: SessionCommand): Promise<boolean> {
    const idempotency_key = createIdempotencyKey();
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "submit_session_command",
      payload: { session_id, command, idempotency_key },
    }));
  }

  async cancelQueuedInput(session_id: SessionId, input_id: InputId): Promise<void> {
    await this.#runQueueMutation(session_id, input_id, () => this.dependencies.runtime.client!.command({
      type: "cancel_queued_input",
      payload: { session_id, input_id },
    }));
  }

  async resumeQueuedInput(session_id: SessionId, input_id: InputId, revision: number): Promise<void> {
    await this.#runQueueMutation(session_id, input_id, () => this.dependencies.runtime.client!.command({
      type: "resume_queued_input",
      payload: { session_id, input_id, expected_revision: revision },
    }));
  }

  async resumeAllQueuedInputs(session_id: SessionId, revision: number): Promise<void> {
    await this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "resume_queued_input",
      payload: { session_id, expected_revision: revision },
    }));
  }

  async interruptRun(session_id: SessionId, run_id: RunId): Promise<void> {
    await this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "interrupt_run",
      payload: { session_id, run_id },
    }));
  }

  async clearWorkPlan(session_id: SessionId, expected_revision: number): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "clear_work_plan",
      payload: { session_id, expected_revision },
    }));
  }

  async stopGoal(session_id: SessionId, goal_id: GoalId, expected_generation: number): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "stop_goal",
      payload: { session_id, goal_id, expected_generation },
    }));
  }

  async resumeGoal(
    session_id: SessionId,
    goal_id: GoalId,
    expected_generation: number,
    input_id: InputId | null = null,
  ): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "resume_goal",
      payload: { session_id, goal_id, expected_generation, input_id },
    }));
  }

  async clearGoal(session_id: SessionId, goal_id: GoalId, expected_generation: number): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "clear_goal",
      payload: { session_id, goal_id, expected_generation },
    }));
  }

  async setSessionReasoningEffort(session_id: SessionId, effort: ReasoningEffortKey | null): Promise<boolean> {
    return this.#runSessionMutation(session_id, () => this.dependencies.runtime.client!.command({
      type: "set_session_reasoning_effort",
      payload: { session_id, effort },
    }));
  }

  async decideApproval(
    session_id: SessionId,
    approval_id: ApprovalId,
    decision: ApprovalDecision,
  ): Promise<void> {
    await this.#runApprovalMutation(session_id, approval_id, () => this.dependencies.runtime.client!.command({
      type: "decide_approval",
      payload: { session_id, approval_id, decision },
    }));
  }

  async rejectApprovalAndStopRun(
    session_id: SessionId,
    approval_id: ApprovalId,
    queue_revision: number,
  ): Promise<void> {
    await this.#runApprovalMutation(session_id, approval_id, () => this.dependencies.runtime.client!.command({
      type: "reject_approval_and_stop_run",
      payload: { session_id, approval_id, expected_queue_revision: queue_revision },
    }));
  }

  async #runSessionMutation(session_id: SessionId, mutation: () => Promise<unknown>): Promise<boolean> {
    const { connection, runtime, state } = this.dependencies;
    if (!runtime.client || connection.state !== "connected" || state.composer_pending) {
      return false;
    }
    state.composer_pending = true;
    state.interaction_error = null;
    try {
      await mutation();
      await runtime.loadSession(session_id);
      await runtime.loadApplication();
      return true;
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
      await runtime.loadSession(session_id);
      return false;
    } finally {
      runInAction(() => {
        state.composer_pending = false;
      });
    }
  }

  async #runQueueMutation(session_id: SessionId, input_id: InputId, mutation: () => Promise<unknown>): Promise<void> {
    const { runtime, state } = this.dependencies;
    if (!runtime.client || state.pending_queue_input_id || this.intervention?.target.type === "queue") {
      return;
    }
    state.pending_queue_input_id = input_id;
    state.interaction_error = null;
    try {
      await mutation();
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
    } finally {
      await runtime.loadSession(session_id);
      runInAction(() => {
        state.pending_queue_input_id = null;
      });
    }
  }

  async #runApprovalMutation(
    session_id: SessionId,
    approval_id: ApprovalId,
    mutation: () => Promise<unknown>,
  ): Promise<void> {
    const { runtime, state } = this.dependencies;
    if (!runtime.client || state.pending_approval_id) {
      return;
    }
    state.pending_approval_id = approval_id;
    state.interaction_error = null;
    try {
      await mutation();
    } catch (error: unknown) {
      runInAction(() => {
        state.interaction_error = displayError(error);
      });
    } finally {
      await runtime.loadSession(session_id);
      runInAction(() => {
        state.pending_approval_id = null;
      });
    }
  }
}

const QUEUE_NOTICES: Record<InterventionNotExecuted, string> = {
  queued: "消息仍在队列。",
  queue_paused: "已停止，消息保留在队列。",
  session_faulted: "上一轮结算失败，消息保留在队列。",
  could_not_interrupt: "未能打断当前执行，消息保留在队列。",
};

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}

function createIdempotencyKey(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `input-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
