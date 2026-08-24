import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApprovalDecision,
  GoalStateSnapshot,
  SessionViewSnapshot,
  SubmitInputMode,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ApprovalWorkspace, isAllowDecision } from "./ApprovalWorkspace";
import {
  formatCompact,
  SLASH_COMMANDS,
  type SlashCommandItem,
} from "./composerOptions";
import { ExecutionSettingsPopover } from "./ExecutionSettingsPopover";
import { GoalStatusRow } from "./GoalStatusRow";
import { ModelSettingsPopover } from "./ModelSettingsPopover";
import { QueueDrawer } from "./QueueDrawer";
import { SlashCommandHelp, SlashCommandMenu } from "./SlashCommandMenu";
import { TodoSummary } from "./TodoSummary";
import { type ComposerAttachment, useComposerAttachments } from "./useComposerAttachments";
import styles from "./index.module.scss";

export const ComposerDock = observer(function ComposerDock({ read_only = false }: Readonly<{
  read_only?: boolean;
}>) {
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const session = session_view?.session;
  const [draft, setDraft] = useState("");
  const [goal_armed, setGoalArmed] = useState(false);
  const [active_overlay, setActiveOverlay] = useState<"todo" | "execution" | "model" | null>(null);
  const [initial_settings_category, setInitialSettingsCategory] = useState<"variant" | "approval" | "model" | "effort" | null>(null);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const [expanded_drawer, setExpandedDrawer] = useState<"goal" | "queue" | null>("queue");
  const [approval_minimized, setApprovalMinimized] = useState(false);
  const [approval_decision, setApprovalDecision] = useState<ApprovalDecision | null>(null);
  const [show_help, setShowHelp] = useState(false);
  const textarea_ref = useRef<HTMLTextAreaElement>(null);
  const slash_ref = useRef<HTMLDivElement>(null);
  const composing_ref = useRef(false);
  const previous_approval_count = useRef(0);
  const previous_approval_session_id = useRef<string | null>(null);
  const attachment_flow = useComposerAttachments({
    disabled: store.composer_pending,
    on_error: (message) => store.showInteractionError(message),
    session_id,
  });

  const model_options = useMemo(() =>
    (application?.models ?? []).flatMap((model) => model.model_key && model.is_valid ? [{
      value: model.model_key,
      label: model.display_name,
      description: [model.provider ?? model.protocol, model.model, model.context_window_tokens ? formatCompact(model.context_window_tokens) : null]
        .filter(Boolean).join(" · "),
    }] : []), [application?.models]);

  const slash_query = draft.startsWith("/") && !draft.includes("\n") ? draft.toLocaleLowerCase() : null;
  const slash_items: readonly SlashCommandItem[] = slash_query === null ? [] : SLASH_COMMANDS
    .filter((item) => item.name.includes(slash_query) || item.description.toLocaleLowerCase().includes(slash_query.slice(1)))
    .map((item) => ({ ...item, disabled_reason: slashDisabledReason(item.name, session_view) }));

  useEffect(() => {
    const first_enabled = slash_items.findIndex((item) => !item.disabled_reason);
    setSlashActiveIndex(Math.max(0, first_enabled));
    if (slash_query !== null) {
      setActiveOverlay(null);
      setShowHelp(false);
    }
  }, [slash_query]);

  useEffect(() => {
    setGoalArmed(false);
    setExpandedDrawer("queue");
    setActiveOverlay(null);
    setShowHelp(false);
  }, [session_id]);

  useEffect(() => {
    const count = session_view?.approvals.items.length ?? 0;
    if (
      count > 0
      && (previous_approval_count.current === 0 || previous_approval_session_id.current !== session_id)
    ) {
      setApprovalMinimized(false);
    }
    previous_approval_count.current = count;
    previous_approval_session_id.current = session_id;
  }, [session_view?.approvals.items.length, session_id]);

  const approval = session_view?.approvals.items[0];
  useEffect(() => {
    if (!approval) {
      setApprovalDecision(null);
      return;
    }
    setApprovalDecision(approval.available_decisions.find(isAllowDecision) ?? null);
  }, [approval?.approval_id]);

  useEffect(() => {
    if (approval && !approval_minimized) {
      setActiveOverlay(null);
      setShowHelp(false);
    }
  }, [approval, approval_minimized]);

  useEffect(() => {
    resizeTextarea(textarea_ref.current);
  }, [draft]);

  if (!session || !session_view) {
    return null;
  }
  if (read_only && !approval) {
    return null;
  }

  const is_archived = session.lifecycle === "archived";
  const is_idle_for_model = !session.active_run_id && session_view.queue.items.length === 0;

  async function submitDraft() {
    const value = draft;
    if (!value.trim() || store.composer_pending || attachment_flow.pending) {
      return;
    }
    if (slash_query) {
      const exact = slash_items.find((item) => item.name === slash_query.trim());
      if (exact) {
        handleSlashCommand(exact);
        return;
      }
    }
    const attachment_ids = await attachment_flow.uploadAll(session!.session_id);
    if (!attachment_ids) {
      return;
    }
    const mode = resolveSubmitMode(goal_armed, session_view!.goal?.state);
    if (await store.submitInput(session!.session_id, value, session!.current_variant, attachment_ids, mode)) {
      setDraft("");
      setGoalArmed(false);
      attachment_flow.clear();
    }
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.disabled_reason) {
      setDraft("");
      store.showInteractionError(command.disabled_reason);
      requestAnimationFrame(() => textarea_ref.current?.focus());
      return;
    }
    if (command.name === "/goal") {
      setDraft("");
      setGoalArmed(true);
      requestAnimationFrame(() => textarea_ref.current?.focus());
      return;
    }
    if (command.picker) {
      setDraft("");
      setInitialSettingsCategory(command.picker);
      setActiveOverlay(command.picker === "model" ? "model" : "execution");
      return;
    }
    if (command.name === "/new") {
      setDraft("");
      void store.createSession(session!.workspace_id);
      return;
    }
    setDraft("");
    setShowHelp(true);
  }

  function handleTextareaKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (composing_ref.current || event.nativeEvent.isComposing) {
      return;
    }
    if (slash_items.length > 0 && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const direction = event.key === "ArrowDown" ? 1 : -1;
      const next = nextEnabledSlashIndex(slash_items, slash_active_index, direction);
      setSlashActiveIndex(next);
      slash_ref.current?.querySelector<HTMLElement>(`[data-slash-index="${next}"]`)?.scrollIntoView({ block: "nearest" });
      return;
    }
    if (event.key === "Escape" && slash_query !== null) {
      event.preventDefault();
      setDraft(draft.slice(1));
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const exact = slash_query === null
        ? undefined
        : slash_items.find((item) => item.name === slash_query.trim());
      if (exact) {
        handleSlashCommand(exact);
        return;
      }
      const command = slash_items[slash_active_index];
      if (command && slash_query !== null) {
        handleSlashCommand(command);
      } else {
        void submitDraft();
      }
    }
  }

  const goal = session_view.goal;
  const goal_stops_from_primary = !draft.trim() && goal?.state === "running";
  const run_interrupts_from_primary = !draft.trim() && !goal_stops_from_primary && Boolean(session.active_run_id);
  const primary_action_available = Boolean(draft.trim()) || goal_stops_from_primary || run_interrupts_from_primary;
  const primary_action = resolvePrimaryAction(goal_stops_from_primary, run_interrupts_from_primary);
  const primary_label = primaryActionLabel(primary_action);
  const execution_initial_category = active_overlay === "execution"
    && (initial_settings_category === "variant" || initial_settings_category === "approval")
    ? initial_settings_category
    : null;
  const model_initial_category = active_overlay === "model"
    && (initial_settings_category === "model" || initial_settings_category === "effort")
    ? initial_settings_category
    : null;

  function updateActiveOverlay(
    overlay: Exclude<typeof active_overlay, null>,
    open: boolean,
  ) {
    setActiveOverlay((current) => {
      if (open) return overlay;
      return current === overlay ? null : current;
    });
  }

  return (
    <div className={styles.dock}>
      {!read_only && session_view.work_plan && (
        <TodoSummary
          on_open_change={(open) => updateActiveOverlay("todo", open)}
          open={active_overlay === "todo"}
          work_plan={session_view.work_plan}
        />
      )}
      {store.interaction_error && (
        <div className={styles.error_notice} role="alert">
          <span>{store.interaction_error}</span>
          <button aria-label="关闭错误提示" onClick={() => store.clearInteractionError()} type="button"><Icon name="x" size={14} /></button>
        </div>
      )}
      {!read_only && goal && (
        <GoalStatusRow
          goal={goal}
          on_clear={() => store.clearGoal(session.session_id, goal.goal_id, goal.generation)}
          on_open_change={(open) => setExpandedDrawer(open ? "goal" : null)}
          on_resume={() => store.resumeGoal(session.session_id, goal.goal_id, goal.generation)}
          on_stop={() => store.stopGoal(session.session_id, goal.goal_id, goal.generation)}
          open={expanded_drawer === "goal"}
          pending={store.composer_pending || is_archived}
        />
      )}
      {approval && approval_minimized && (
        <button className={styles.approval_restore} onClick={() => setApprovalMinimized(false)} type="button">
          <Icon name="shield" size={16} />
          <span>等待审批</span>
          <b>{session_view.approvals.items.length}</b>
          <Icon name="chevron-down" size={14} />
        </button>
      )}
      {!read_only && session_view.queue.items.length > 0 && (
        <QueueDrawer
          goal={goal}
          open={expanded_drawer === "queue"}
          on_open_change={(open) => setExpandedDrawer(open ? "queue" : null)}
          queue={session_view.queue}
          session_id={session.session_id}
        />
      )}
      {approval && !approval_minimized ? (
        <ApprovalWorkspace
          approval={approval}
          child_title={approval.child_task_id
            ? session_view.child_tasks.find((item) => item.task.child_task_id === approval.child_task_id)?.task.title ?? null
            : null}
          decision={approval_decision}
          on_decision_change={setApprovalDecision}
          on_minimize={() => setApprovalMinimized(true)}
          queue_revision={session_view.queue.revision}
          remaining={session_view.approvals.items.length - 1}
        />
      ) : read_only ? null : is_archived ? (
        <div className={styles.archived_notice}>
          <span>此会话已归档，只能查看历史内容。</span>
          <button disabled={store.composer_pending} onClick={() => void store.restoreSession(session.session_id)} type="button">恢复会话</button>
        </div>
      ) : (
        <section className={styles.composer}>
          {slash_items.length > 0 && (
            <SlashCommandMenu active_index={slash_active_index} items={slash_items} menu_ref={slash_ref} on_select={handleSlashCommand} />
          )}
          {show_help && <SlashCommandHelp on_close={() => setShowHelp(false)} />}
          {goal_armed && (
            <div className={styles.draft_tags}>
              <span>
                Goal
                <button aria-label="取消 Goal 标记" onClick={() => setGoalArmed(false)} type="button"><Icon name="x" size={12} /></button>
              </span>
            </div>
          )}
          {attachment_flow.attachments.length > 0 && (
            <div aria-label="待发送附件" className={styles.attachment_list}>
              {attachment_flow.attachments.map((attachment) => (
                <div data-state={attachment.state} key={attachment.selection_id} title={attachment.error ?? attachment.original_name}>
                  <Icon name="paperclip" size={14} />
                  <span>{attachment.original_name}</span>
                  <small>{attachmentStateLabel(attachment)}</small>
                  <button
                    aria-label={`移除附件 ${attachment.original_name}`}
                    title={attachment.state === "uploading" ? "取消上传" : "移除附件"}
                    onClick={() => attachment_flow.remove(attachment)}
                    type="button"
                  >
                    <Icon name="x" size={13} />
                  </button>
                </div>
              ))}
            </div>
          )}
          <textarea
            aria-label="输入消息"
            disabled={store.connection.state !== "connected" || store.composer_pending || attachment_flow.pending}
            onChange={(event) => setDraft(event.target.value)}
            onCompositionEnd={() => { composing_ref.current = false; }}
            onCompositionStart={() => { composing_ref.current = true; }}
            onKeyDown={handleTextareaKeyDown}
            placeholder={goal?.state === "paused" ? "输入新指导并恢复 Goal…" : "输入消息…  / 使用指令"}
            ref={textarea_ref}
            rows={2}
            value={draft}
          />
          <footer className={styles.composer_actions}>
            <Tooltip content="添加附件">
              <button
                aria-label="添加附件"
                className={styles.icon_button}
                disabled={store.composer_pending || attachment_flow.pending}
                onClick={() => void attachment_flow.choose()}
                type="button"
              >
                <Icon name="plus" size={17} />
              </button>
            </Tooltip>
            <ExecutionSettingsPopover
              approval_mode={session.approval_mode}
              disabled={store.composer_pending}
              initial_category={execution_initial_category}
              on_approval_change={(mode) => store.setSessionApprovalMode(session.session_id, mode)}
              on_open_change={(open) => {
                setShowHelp(false);
                if (open) setInitialSettingsCategory(null);
                updateActiveOverlay("execution", open);
              }}
              on_variant_change={(variant) => store.setSessionVariant(session.session_id, variant)}
              open={active_overlay === "execution"}
              trigger_class_name={styles.execution_selector}
              variant={session.current_variant}
            />
            <span className={styles.action_spacer} />
            <ContextUsageRing view={session_view} />
            <ModelSettingsPopover
              disabled={store.composer_pending}
              effort={session.reasoning_effort ?? null}
              effort_options={session_view.composer_capabilities.reasoning_effort_options}
              initial_category={model_initial_category}
              model_display_name={modelDisplayName(application, session.model_key)}
              model_key={session.model_key}
              model_options={model_options}
              model_switch_disabled_reason={is_idle_for_model ? undefined : "存在活动 Run 或排队输入时不能切换模型"}
              on_effort_change={(effort) => store.setSessionReasoningEffort(session.session_id, effort)}
              on_model_change={(model_key) => store.setSessionModel(session.session_id, model_key)}
              on_open_change={(open) => {
                setShowHelp(false);
                if (open) setInitialSettingsCategory(null);
                updateActiveOverlay("model", open);
              }}
              open={active_overlay === "model"}
              trigger_class_name={styles.model_selector}
            />
            <button
              aria-label={draft.trim() ? "发送消息" : primary_label}
              className={styles.send_button}
              data-action={primary_action}
              disabled={store.composer_pending || attachment_flow.pending || !primary_action_available}
              onClick={() => {
                if (draft.trim()) {
                  void submitDraft();
                } else if (goal_stops_from_primary && goal) {
                  void store.stopGoal(session.session_id, goal.goal_id, goal.generation);
                } else if (session.active_run_id) {
                  void store.interruptRun(session.session_id, session.active_run_id);
                }
              }}
              type="button"
            >
              <Icon name={goal_stops_from_primary || run_interrupts_from_primary ? "stop" : "arrow-down"} size={16} />
            </button>
          </footer>
        </section>
      )}
    </div>
  );
});

function ContextUsageRing({ view }: Readonly<{ view: SessionViewSnapshot }>) {
  const context = view.usage.context;
  const degrees = context ? Math.min(360, Math.max(0, context.usage_basis_points * 0.036)) : 0;
  const label = context ? `${formatCompact(context.used_tokens)} / ${formatCompact(context.window_tokens)} · ${(context.usage_basis_points / 100).toFixed(1)}%` : "暂无用量数据";
  return <span aria-label={`上下文用量：${label}`} className={styles.context_ring} role="img" style={{ "--context-degrees": `${degrees}deg` } as React.CSSProperties} tabIndex={0}><span>{label}</span></span>;
}

function resizeTextarea(textarea: HTMLTextAreaElement | null) {
  if (!textarea) return;
  textarea.style.height = "auto";
  const line_height = 23;
  textarea.style.height = `${Math.min(line_height * 8, Math.max(line_height * 2, textarea.scrollHeight))}px`;
}

function modelDisplayName(application: ReturnType<typeof useRootStore>["projection"]["application"], model_key: string): string {
  return application?.models.find((model) => model.model_key === model_key)?.display_name ?? model_key;
}

function attachmentStateLabel(attachment: ComposerAttachment): string {
  switch (attachment.state) {
    case "selected":
      return formatBytes(attachment.size_bytes);
    case "uploading":
      return "上传中";
    case "uploaded":
      return "已上传";
    case "failed":
      return "重试发送";
  }
}

function formatBytes(value: number): string {
  if (value >= 1024 * 1024) {
    return `${(value / (1024 * 1024)).toFixed(1)} MB`;
  }
  if (value >= 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${value} B`;
}

function slashDisabledReason(
  command_name: string,
  view: SessionViewSnapshot | undefined,
): string | null {
  if (command_name !== "/goal") return null;
  if (view?.goal) return "当前会话已有 Goal，请先继续或退出现有 Goal";
  return null;
}

function resolveSubmitMode(
  goal_armed: boolean,
  goal_state: GoalStateSnapshot | undefined,
): SubmitInputMode {
  if (goal_armed) return "start_goal";
  if (goal_state === "paused") return "resume_goal";
  return "normal";
}

function nextEnabledSlashIndex(
  items: readonly SlashCommandItem[],
  current: number,
  direction: 1 | -1,
): number {
  let next = current;
  for (let checked = 0; checked < items.length; checked += 1) {
    next = (next + direction + items.length) % items.length;
    if (!items[next]?.disabled_reason) return next;
  }
  return current;
}

function primaryActionLabel(action: "send" | "stop-goal" | "interrupt"): string {
  if (action === "stop-goal") return "停止 Goal";
  if (action === "interrupt") return "中断当前轮次";
  return "发送消息";
}

function resolvePrimaryAction(
  stop_goal: boolean,
  interrupt_run: boolean,
): "send" | "stop-goal" | "interrupt" {
  if (stop_goal) return "stop-goal";
  if (interrupt_run) return "interrupt";
  return "send";
}
