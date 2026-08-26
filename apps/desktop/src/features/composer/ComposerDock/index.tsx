import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApprovalDecision,
  GoalStateSnapshot,
  SessionViewSnapshot,
  SkillSummarySnapshot,
  SubmitInputMode,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useInputMethodGuard } from "../../../components/InputMethodGuard";
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
import { queuePresentation } from "./queuePresentation";
import { SlashCommandHelp, SlashCommandMenu } from "./SlashCommandMenu";
import { SkillPicker } from "./SkillPicker";
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
  const [selected_skill, setSelectedSkill] = useState<SkillSummarySnapshot | null>(null);
  const [skill_picker_open, setSkillPickerOpen] = useState(false);
  const [active_overlay, setActiveOverlay] = useState<"todo" | "execution" | "model" | null>(null);
  const [initial_settings_category, setInitialSettingsCategory] = useState<"variant" | "approval" | "model" | "effort" | null>(null);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const [expanded_drawer, setExpandedDrawer] = useState<"goal" | "queue" | null>("queue");
  const [approval_minimized, setApprovalMinimized] = useState(false);
  const [approval_decision, setApprovalDecision] = useState<ApprovalDecision | null>(null);
  const [show_help, setShowHelp] = useState(false);
  const textarea_ref = useRef<HTMLTextAreaElement>(null);
  const slash_ref = useRef<HTMLDivElement>(null);
  const input_method = useInputMethodGuard();
  const previous_approval_count = useRef(0);
  const previous_approval_session_id = useRef<string | null>(null);
  const attachment_flow = useComposerAttachments({
    disabled: store.composer_pending || store.pending_compaction_session_id === session_id,
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
  }, [slash_query]);

  useEffect(() => {
    setGoalArmed(false);
    setSelectedSkill(null);
    setSkillPickerOpen(false);
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
  const selected_model_key = session_view.composer_capabilities.selected_model_key ?? null;
  const model_required = selected_model_key === null;
  const queue_presentation = queuePresentation(session_view.queue, session.active_run_id);
  const manual_compaction = session.active_compaction?.trigger.type === "manual"
    ? session.active_compaction
    : null;
  const compaction_command_pending = store.pending_compaction_session_id === session.session_id;

  async function submitCompaction() {
    if (draft.trim() !== "/compact") {
      return;
    }
    setDraft("");
    if (attachment_flow.attachments.length > 0 || goal_armed || selected_skill) {
      store.showInteractionError("/compact 必须单独使用，请先移除附件、目标和技能标记。");
      return;
    }
    if (
      session!.active_run_id
      || session!.queued_input_count > 0
      || session!.pending_approval_count > 0
      || session!.active_child_count > 0
      || session!.active_compaction
      || compaction_command_pending
      || store.pending_session_action
    ) {
      store.showInteractionError("当前会话正忙，暂时不能压缩上下文。");
      return;
    }
    await store.compactSession(
      session!.session_id,
      session_view!.conversation_generation,
    );
  }

  async function submitDraft() {
    const value = draft;
    if (value.trim() === "/compact") {
      await submitCompaction();
      return;
    }
    if (!value.trim() || model_required || store.composer_pending || attachment_flow.pending) {
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
    const submitted = selected_skill
      ? await store.submitInput(
        session!.session_id,
        value,
        session!.current_variant,
        attachment_ids,
        mode,
        selected_skill.name,
      )
      : await store.submitInput(session!.session_id, value, session!.current_variant, attachment_ids, mode);
    if (submitted) {
      setDraft("");
      setGoalArmed(false);
      setSelectedSkill(null);
      attachment_flow.clear();
    }
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.name === "/compact") {
      if (draft.trim() === "/compact") {
        void submitCompaction();
      } else {
        setDraft("/compact");
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
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
    if (command.name === "/skill") {
      setDraft("");
      setSkillPickerOpen(true);
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
    if (input_method.shouldIgnoreKeyDown(event)) {
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

  function handleDraftChange(value: string) {
    setDraft(value);
    if (value.startsWith("/") && !value.includes("\n")) {
      setActiveOverlay(null);
      setShowHelp(false);
    }
  }

  const goal = session_view.goal;
  const compaction_cancels_from_primary = Boolean(manual_compaction?.cancellable);
  const goal_stops_from_primary = !compaction_cancels_from_primary && !draft.trim() && goal?.state === "running";
  const run_interrupts_from_primary = !draft.trim() && !goal_stops_from_primary && Boolean(session.active_run_id);
  const primary_action_available = compaction_cancels_from_primary || Boolean(draft.trim()) || goal_stops_from_primary || run_interrupts_from_primary;
  const primary_action = resolvePrimaryAction(compaction_cancels_from_primary, goal_stops_from_primary, run_interrupts_from_primary);
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
      {!read_only && session_view.work_plan && session_view.work_plan.items.length > 0 && (
        <TodoSummary
          on_open_change={(open) => updateActiveOverlay("todo", open)}
          open={active_overlay === "todo"}
          running={Boolean(session.active_run_id)}
          work_plan={session_view.work_plan}
        />
      )}
      {store.interaction_error && (
        <div className={styles.error_notice} role="alert">
          <span>{store.interaction_error}</span>
          <button aria-label="关闭错误提示" onClick={() => store.clearInteractionError()} type="button"><Icon name="x" size={14} /></button>
        </div>
      )}
      {store.session_notice?.session_id === session.session_id && (
        <div className={styles.session_notice} data-tone={store.session_notice.tone} role="status">
          <span>{store.session_notice.message}</span>
          <button aria-label="关闭状态提示" onClick={() => store.clearSessionNotice(session.session_id)} type="button"><Icon name="x" size={14} /></button>
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
      {!read_only && queue_presentation.visible && (
        <QueueDrawer
          goal={goal}
          open={expanded_drawer === "queue"}
          on_open_change={(open) => setExpandedDrawer(open ? "queue" : null)}
          queue={session_view.queue}
          presentation={queue_presentation}
          session_id={session.session_id}
        />
      )}
      {!read_only && session.active_compaction && (
        <div className={styles.compaction_status} role="status">
          <span className={styles.loading_ring} />
          <strong>{session.active_compaction.trigger.type === "manual" ? "正在压缩上下文" : "正在自动压缩上下文"}</strong>
        </div>
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
          {session.proxy && (
            <div className={styles.proxy_takeover_hint}>当前由主控代理；发送消息后将由你接管并退出代理。</div>
          )}
          {slash_items.length > 0 && (
            <SlashCommandMenu active_index={slash_active_index} items={slash_items} menu_ref={slash_ref} on_select={handleSlashCommand} />
          )}
          {show_help && <SlashCommandHelp on_close={() => setShowHelp(false)} />}
          {skill_picker_open && (
            <SkillPicker
              on_close={() => {
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              on_select={(skill) => {
                setSelectedSkill(skill);
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              skills={availableUserSkills(session_view.skill_catalog.skills)}
            />
          )}
          {(goal_armed || selected_skill) && (
            <div className={styles.draft_tags}>
              {goal_armed && <span>
                目标
                <button aria-label="取消目标标记" onClick={() => setGoalArmed(false)} type="button"><Icon name="x" size={12} /></button>
              </span>}
              {selected_skill && <span title={selected_skill.name}>
                {selected_skill.name}
                <button aria-label={`移除技能 ${selected_skill.name}`} onClick={() => setSelectedSkill(null)} type="button"><Icon name="x" size={12} /></button>
              </span>}
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
            disabled={model_required || store.connection.state !== "connected" || store.composer_pending || attachment_flow.pending || Boolean(manual_compaction) || compaction_command_pending}
            onChange={(event) => handleDraftChange(event.target.value)}
            onCompositionEnd={input_method.onCompositionEnd}
            onCompositionStart={input_method.onCompositionStart}
            onKeyDown={handleTextareaKeyDown}
            onKeyUp={input_method.onKeyUp}
            placeholder={model_required
              ? "请先选择一个可用模型"
              : manual_compaction || compaction_command_pending
                ? "正在压缩上下文…"
              : goal?.state === "paused"
                ? "输入新指导并恢复目标…"
                : "输入消息…  / 使用指令"}
            ref={textarea_ref}
            rows={2}
            value={draft}
          />
          <footer className={styles.composer_actions}>
            <Tooltip content="添加附件">
              <button
                aria-label="添加附件"
                className={styles.icon_button}
                disabled={store.composer_pending || attachment_flow.pending || Boolean(manual_compaction) || compaction_command_pending}
                onClick={() => void attachment_flow.choose()}
                type="button"
              >
                <Icon name="plus" size={17} />
              </button>
            </Tooltip>
            <ExecutionSettingsPopover
              approval_mode={session.approval_mode}
              disabled={store.composer_pending || Boolean(manual_compaction) || compaction_command_pending}
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
              disabled={store.composer_pending || Boolean(manual_compaction) || compaction_command_pending}
              effort={session.reasoning_effort ?? null}
              effort_options={session_view.composer_capabilities.reasoning_effort_options}
              initial_category={model_initial_category}
              model_display_name={selected_model_key
                ? modelDisplayName(application, selected_model_key)
                : "未选择模型"}
              model_key={selected_model_key}
              model_options={model_options}
              model_switch_disabled_reason={is_idle_for_model ? undefined : "存在活动运行或排队输入时不能切换模型"}
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
              aria-label={draft.trim() && !compaction_cancels_from_primary ? "发送消息" : primary_label}
              className={styles.send_button}
              data-action={primary_action}
              disabled={(store.composer_pending && !compaction_cancels_from_primary)
                || attachment_flow.pending
                || !primary_action_available
                || store.pending_compaction_cancel_session_id === session.session_id
                || (Boolean(draft.trim()) && model_required && !compaction_cancels_from_primary)}
              onClick={() => {
                if (compaction_cancels_from_primary && manual_compaction) {
                  void store.cancelSessionCompaction(session.session_id, manual_compaction.compaction_id);
                } else if (draft.trim()) {
                  void submitDraft();
                } else if (goal_stops_from_primary && goal) {
                  void store.stopGoal(session.session_id, goal.goal_id, goal.generation);
                } else if (session.active_run_id) {
                  void store.interruptRun(session.session_id, session.active_run_id);
                }
              }}
              type="button"
            >
              <Icon name={compaction_cancels_from_primary || goal_stops_from_primary || run_interrupts_from_primary ? "stop" : "arrow-down"} size={16} />
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
  if (command_name === "/skill") {
    if (!view || view.skill_catalog.status === "unavailable" || view.skill_catalog.status === "legacy_unavailable") {
      return "当前会话的技能信息不可用";
    }
    if (availableUserSkills(view.skill_catalog.skills).length === 0) return "当前会话没有用户可选技能";
    return null;
  }
  if (command_name !== "/goal") return null;
  if (view?.goal) return "当前会话已有目标，请先继续或退出现有目标";
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

function primaryActionLabel(action: "send" | "stop-goal" | "interrupt" | "cancel-compaction"): string {
  if (action === "cancel-compaction") return "终止压缩";
  if (action === "stop-goal") return "停止目标";
  if (action === "interrupt") return "中断当前轮次";
  return "发送消息";
}

function availableUserSkills(skills: readonly SkillSummarySnapshot[]): SkillSummarySnapshot[] {
  return skills.filter((skill) => skill.enabled && skill.user_invocable && skill.health === "ready");
}

function resolvePrimaryAction(
  cancel_compaction: boolean,
  stop_goal: boolean,
  interrupt_run: boolean,
): "send" | "stop-goal" | "interrupt" | "cancel-compaction" {
  if (cancel_compaction) return "cancel-compaction";
  if (stop_goal) return "stop-goal";
  if (interrupt_run) return "interrupt";
  return "send";
}
