import { runInAction } from "mobx";
import type {
  AgentVariant,
  ApprovalDecision,
  ApprovalId,
  AttachmentId,
  ChildTaskId,
  GoalId,
  InputId,
  McpServerKey,
  RunId,
  ReasoningEffortKey,
  QuotedTextSnapshot,
  SessionId,
  SessionCommand,
  SubmitInputMode,
} from "../generated/assistant-protocol";
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
}>;

/** Owns input, queue, approval and active-run command workflows. */
export class RunInteractionController {
  constructor(private readonly dependencies: RunInteractionDependencies) {}

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
  ): Promise<boolean> {
    const { connection, runtime, state } = this.dependencies;
    const client = runtime.client;
    if (
      !client
      || connection.state !== "connected"
      || state.composer_pending
      || (!message.trim() && attachment_ids.length === 0 && quotes.length === 0)
    ) {
      return false;
    }
    state.composer_pending = true;
    state.interaction_error = null;
    try {
      await client.command({
        type: "submit_input",
        payload: {
          session_id,
          message,
          variant,
          mode,
          attachment_ids: [...attachment_ids],
          quotes: [...quotes],
          skill_name: skill_name ?? undefined,
          mcp_server_key: mcp_server_key ?? undefined,
          idempotency_key: createIdempotencyKey(),
        },
      });
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
    if (!runtime.client || state.pending_queue_input_id) {
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

function displayError(error: unknown): string {
  return error instanceof Error ? error.message : "无法连接本地 Runtime。";
}

function createIdempotencyKey(): string {
  return typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : `input-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}
