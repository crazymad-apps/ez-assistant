import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApprovalDecision,
  GoalStateSnapshot,
  McpSelectionTagSnapshot,
  QuotedTextSnapshot,
  SessionId,
  SessionViewSnapshot,
  SkillSummarySnapshot,
  SubmitInputMode,
} from "../../../generated/assistant-protocol";
import { Button } from "../../../components/Button";
import { Icon } from "../../../components/Icon";
import { PresenceBoundary } from "../../../components/Presence";
import { useInputMethodGuard } from "../../../components/InputMethodGuard";
import { Tooltip } from "../../../components/Tooltip";
import type { NewSessionDraft, NewSessionDraftKey } from "../../../stores/NewSessionDraftStore";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ApprovalWorkspace, isAllowDecision } from "./ApprovalWorkspace";
import { AttachmentDetailDialog } from "./AttachmentDetailDialog";
import { QuoteDetailDialog } from "../../conversation/QuoteDetailDialog";
import {
  formatCompact,
  SLASH_COMMANDS,
  type SlashCommandItem,
} from "./composerOptions";
import { ExecutionSettingsPopover } from "./ExecutionSettingsPopover";
import { GoalStatusRow } from "./GoalStatusRow";
import { ModelSettingsPopover } from "./ModelSettingsPopover";
import { OutputHostingMenu } from "./OutputHostingMenu";
import { QueueDrawer } from "./QueueDrawer";
import { queuePresentation } from "./queuePresentation";
import { parseMcpRefreshCommand } from "./sessionCommand";
import { SlashCommandHelp, SlashCommandMenu } from "./SlashCommandMenu";
import { InputContextPicker } from "./InputContextPicker";
import { McpServerPicker } from "./McpServerPicker";
import { TodoSummary } from "./TodoSummary";
import { type ComposerAttachment, useComposerAttachments } from "./useComposerAttachments";
import styles from "./index.module.scss";

export const ComposerDock = observer(function ComposerDock({ read_only = false }: Readonly<{
  read_only?: boolean;
}>) {
  const store = useRootStore();
  const draft_key = store.navigation.selected_draft_key;
  if (draft_key) {
    return read_only ? null : <NewSessionDraftComposer draft_key={draft_key} />;
  }
  return <SessionComposerDock read_only={read_only} />;
});

const SessionComposerDock = observer(function SessionComposerDock({ read_only = false }: Readonly<{
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
  const [selected_mcp, setSelectedMcp] = useState<McpSelectionTagSnapshot | null>(null);
  const [mcp_picker_open, setMcpPickerOpen] = useState(false);
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
  const input_owner = useRef({ session_id, variant: session?.current_variant });
  // 上传与提交可能跨越导航；旧输入不能清除新页面刚选中的标签。
  if (input_owner.current.session_id !== session_id || input_owner.current.variant !== session?.current_variant) {
    input_owner.current = { session_id, variant: session?.current_variant };
  }
  const attachment_flow = useComposerAttachments({
    disabled: store.composer_pending || store.pending_compaction_session_id === session_id,
    on_error: (message) => store.showInteractionError(message),
    owner_key: session_id,
  });
  const quotes = store.composer_quotes.get(session_id);

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
    .map((item) => ({ ...item, disabled_reason: slashDisabledReason(item.name, session_view, application?.capabilities.session_commands ?? false) }));

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
    setSelectedMcp(null);
    setMcpPickerOpen(false);
  }, [session_id, session?.current_variant]);

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
    if (attachment_flow.attachments.length > 0 || quotes.length > 0 || goal_armed || selected_skill || selected_mcp) {
      store.showInteractionError("/compact 必须单独使用，请先移除附件、引用和标签。");
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
    const owner = input_owner.current;
    const value = draft;
    const control = parseMcpRefreshCommand(value);
    if (control.type === "invalid") {
      store.showInteractionError(control.message);
      return;
    }
    if (control.type === "command") {
      if (attachment_flow.attachments.length > 0 || quotes.length > 0 || selected_skill || goal_armed || selected_mcp) {
        store.showInteractionError("刷新指令不能同时携带附件、引用或标签，请先移除这些内容");
        return;
      }
      if (await store.submitSessionCommand(session!.session_id, control.command)) setDraft("");
      return;
    }
    if (value.trim() === "/compact") {
      await submitCompaction();
      return;
    }
    if (value.trim() === "/title") {
      setDraft("");
      await store.generateSessionTitle(session!.session_id);
      return;
    }
    if ((!value.trim() && attachment_flow.attachments.length === 0 && quotes.length === 0) || model_required || store.composer_pending || attachment_flow.pending) {
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
    if (!attachment_ids || owner !== input_owner.current) {
      return;
    }
    const mode = resolveSubmitMode(goal_armed, session_view!.goal?.state);
    const submitted = await store.submitInput(
      session!.session_id, value, session!.current_variant, attachment_ids, mode,
      selected_skill?.name ?? null, quotes, selected_mcp?.server_key ?? null,
    );
    if (submitted && owner === input_owner.current) {
      setDraft("");
      setGoalArmed(false);
      setSelectedSkill(null);
      setSelectedMcp(null);
      attachment_flow.clear();
      store.composer_quotes.clear(session!.session_id);
    }
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.name === "/mcp refresh") {
      if (command.disabled_reason) {
        store.showInteractionError(command.disabled_reason);
      } else if (draft.trim() === command.name) {
        void submitDraft();
      } else {
        setDraft(command.name);
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
    if (command.name === "/compact") {
      if (draft.trim() === "/compact") {
        void submitCompaction();
      } else {
        setDraft("/compact");
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
    if (command.name === "/title") {
      if (command.disabled_reason) {
        setDraft("");
        store.showInteractionError(command.disabled_reason);
      } else if (draft.trim() === "/title") {
        setDraft("");
        void store.generateSessionTitle(session!.session_id);
      } else {
        setDraft("/title");
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
    if (command.name === "/mcp") {
      setDraft("");
      setSkillPickerOpen(false);
      setMcpPickerOpen(true);
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
      store.openNewSessionDraft(session!.workspace_id);
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
      const exact_command = SLASH_COMMANDS.find(
        (item) => item.name === event.currentTarget.value.trim().toLocaleLowerCase(),
      );
      const exact = exact_command
        ? { ...exact_command, disabled_reason: slashDisabledReason(exact_command.name, session_view, application?.capabilities.session_commands ?? false) }
        : undefined;
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
  const has_composer_content = Boolean(draft.trim() || attachment_flow.attachments.length || quotes.length);
  const goal_stops_from_primary = !compaction_cancels_from_primary && !has_composer_content && goal?.state === "running";
  const run_interrupts_from_primary = !has_composer_content && !goal_stops_from_primary && Boolean(session.active_run_id);
  const primary_action_available = compaction_cancels_from_primary || has_composer_content || goal_stops_from_primary || run_interrupts_from_primary;
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
          <div className={styles.session_notice_actions}>
            {store.session_notice.action === "retry_title" && (
              <button className={styles.session_notice_action} onClick={() => void store.generateSessionTitle(session.session_id)} type="button">重试</button>
            )}
            <button aria-label="关闭状态提示" onClick={() => store.clearSessionNotice(session.session_id)} type="button"><Icon name="x" size={14} /></button>
          </div>
        </div>
      )}
      {!read_only && session_view.title_generation?.trigger === "manual" && (
        <div className={styles.compaction_status} role="status">
          <span className={styles.loading_ring} />
          <strong>正在生成标题</strong>
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
          <SlashCommandMenu active_index={slash_active_index} items={slash_items} menu_ref={slash_ref} on_select={handleSlashCommand} open={slash_items.length > 0} />
          <SlashCommandHelp on_close={() => setShowHelp(false)} open={show_help} />
            <InputContextPicker
              label="技能"
              on_close={() => {
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              on_select={(skill) => {
                setSelectedSkill(skill);
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              open={skill_picker_open}
              options={availableUserSkills(session_view.skill_catalog.skills)}
            />
          {mcp_picker_open && <McpServerPicker
            key={`${session.session_id}:${session.current_variant}`}
            on_close={() => { setMcpPickerOpen(false); requestAnimationFrame(() => textarea_ref.current?.focus()); }}
            on_select={(server) => { setSelectedMcp(server); setMcpPickerOpen(false); requestAnimationFrame(() => textarea_ref.current?.focus()); }}
            request={{ context: { type: "session", payload: { session_id: session.session_id } }, variant: session.current_variant }}
          />}
          <ComposerAttachmentContext
            attachments={attachment_flow.attachments}
            on_quote_locate={(quote) => store.locateTextQuoteSource(session.session_id, quote)}
            on_quote_remove={(quote) => store.composer_quotes.remove(session.session_id, quote.quote_id)}
            on_remove={attachment_flow.remove}
            owner_key={session.session_id}
            paste_pending={attachment_flow.paste_pending}
            quotes={quotes}
            session_id={session.session_id}
          />
          {(goal_armed || selected_skill || selected_mcp) && (
            <div className={styles.draft_tags}>
              {goal_armed && <span>
                目标
                <button aria-label="取消目标标记" onClick={() => setGoalArmed(false)} type="button"><Icon name="x" size={12} /></button>
              </span>}
              {selected_skill && <span title={selected_skill.name}>
                {selected_skill.name}
                <button aria-label={`移除技能 ${selected_skill.name}`} onClick={() => setSelectedSkill(null)} type="button"><Icon name="x" size={12} /></button>
              </span>}
              {selected_mcp && <span title={selected_mcp.server_key}>
                MCP · {selected_mcp.display_name}
                <button aria-label={`移除 MCP ${selected_mcp.display_name}`} onClick={() => setSelectedMcp(null)} type="button"><Icon name="x" size={12} /></button>
              </span>}
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
            onPaste={(event) => handleComposerPaste(event, draft, setDraft, attachment_flow.pasteImages)}
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
              <Button
                aria-label="添加附件"
                disabled={store.composer_pending || attachment_flow.pending || Boolean(manual_compaction) || compaction_command_pending}
                iconOnly
                onClick={() => void attachment_flow.choose()}
                variant="outlined"
              >
                <Icon name="plus" size={17} />
              </Button>
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
            {session.role === "controller" && <OutputHostingMenu session={session} />}
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
              aria-label={has_composer_content && !compaction_cancels_from_primary ? "发送消息" : primary_label}
              className={styles.send_button}
              data-action={primary_action}
              disabled={(store.composer_pending && !compaction_cancels_from_primary)
                || attachment_flow.pending
                || !primary_action_available
                || store.pending_compaction_cancel_session_id === session.session_id
                || (has_composer_content && model_required && !compaction_cancels_from_primary)}
              onClick={() => {
                if (compaction_cancels_from_primary && manual_compaction) {
                  void store.cancelSessionCompaction(session.session_id, manual_compaction.compaction_id);
                } else if (has_composer_content) {
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

const NewSessionDraftComposer = observer(function NewSessionDraftComposer({ draft_key }: Readonly<{
  draft_key: NewSessionDraftKey;
}>) {
  const store = useRootStore();
  const application = store.projection.application;
  const draft = store.new_session_drafts.get(draft_key);
  const [skill_picker_open, setSkillPickerOpen] = useState(false);
  const [mcp_picker_open, setMcpPickerOpen] = useState(false);
  const [active_overlay, setActiveOverlay] = useState<"execution" | "model" | null>(null);
  const [show_help, setShowHelp] = useState(false);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const textarea_ref = useRef<HTMLTextAreaElement>(null);
  const slash_ref = useRef<HTMLDivElement>(null);
  const input_method = useInputMethodGuard();
  const attachment_flow = useComposerAttachments({
    attachments: draft?.attachments ?? [],
    disabled: store.composer_pending,
    on_attachments_change: (attachments) => store.new_session_drafts.updateAttachments(draft_key, attachments),
    on_error: (message) => store.showInteractionError(message),
    owner_key: draft_key,
  });
  const model_options = useMemo(() =>
    (application?.models ?? []).flatMap((model) => model.model_key && model.is_valid ? [{
      value: model.model_key,
      label: model.display_name,
      description: [model.provider ?? model.protocol, model.model, model.context_window_tokens ? formatCompact(model.context_window_tokens) : null]
        .filter(Boolean).join(" · "),
    }] : []), [application?.models]);
  const selected_skill = draft?.skill_options.find((skill) => skill.name === draft.selected_skill_name) ?? null;
  const slash_query = draft?.text.startsWith("/") && !draft.text.includes("\n")
    ? draft.text.toLocaleLowerCase()
    : null;
  const slash_items: readonly SlashCommandItem[] = slash_query === null ? [] : SLASH_COMMANDS
    .filter((item) => item.name.includes(slash_query) || item.description.toLocaleLowerCase().includes(slash_query.slice(1)))
    .map((item) => ({ ...item, disabled_reason: draftSlashDisabledReason(item.name, draft) }));

  useEffect(() => {
    const first_enabled = slash_items.findIndex((item) => !item.disabled_reason);
    setSlashActiveIndex(Math.max(0, first_enabled));
  }, [slash_query]);
  useEffect(() => {
    setSkillPickerOpen(false);
    setActiveOverlay(null);
    setShowHelp(false);
  }, [draft_key]);
  useEffect(() => {
    setMcpPickerOpen(false);
  }, [draft_key, draft?.variant]);
  useEffect(() => () => {
    // 未提交草稿离开 owner 时清理 MCP；结果未知的物化请求必须保留原幂等键和冻结载荷。
    const leaving = store.new_session_drafts.get(draft_key);
    if (leaving?.selected_mcp && !leaving.materialization_attempt) store.new_session_drafts.updateSelectedMcp(draft_key, null);
  }, [draft_key, store]);
  useEffect(() => resizeTextarea(textarea_ref.current), [draft?.text]);

  if (!draft) return null;
  const current_draft = draft;
  const model_required = draft.model_key === null;
  const can_send = Boolean(draft.text.trim() || draft.attachments.length || draft.quotes.length);

  function updateOverlay(overlay: "execution" | "model", open: boolean) {
    setActiveOverlay((current) => open ? overlay : current === overlay ? null : current);
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.disabled_reason) {
      store.new_session_drafts.updateText(draft_key, "");
      store.showInteractionError(command.disabled_reason);
      return;
    }
    store.new_session_drafts.updateText(draft_key, "");
    if (command.name === "/goal") {
      store.new_session_drafts.updateGoalArmed(draft_key, true);
    } else if (command.name === "/skill") {
      setSkillPickerOpen(true);
      return;
    } else if (command.name === "/mcp") {
      setSkillPickerOpen(false);
      setMcpPickerOpen(true);
      return;
    } else if (command.name === "/model") {
      setActiveOverlay("model");
    } else if (command.name === "/mode" || command.name === "/approval") {
      setActiveOverlay("execution");
    } else if (command.name === "/new") {
      store.openNewSessionDraft(current_draft.workspace_id);
    } else {
      setShowHelp(true);
    }
    requestAnimationFrame(() => textarea_ref.current?.focus());
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) return;
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
      store.new_session_drafts.updateText(draft_key, current_draft.text.slice(1));
      return;
    }
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      const exact_command = SLASH_COMMANDS.find(
        (item) => item.name === event.currentTarget.value.trim().toLocaleLowerCase(),
      );
      const command = exact_command
        ? { ...exact_command, disabled_reason: draftSlashDisabledReason(exact_command.name, draft) }
        : slash_query === null ? undefined : slash_items[slash_active_index];
      if (command) handleSlashCommand(command);
      else submitNewDraft();
    }
  }

  function submitNewDraft() {
    const command = slash_items.find((item) => item.name === current_draft.text.trim().toLocaleLowerCase());
    if (command) { handleSlashCommand(command); return; }
    if (parseMcpRefreshCommand(current_draft.text).type !== "not_command") {
      store.showInteractionError("发送第一条消息后，可在会话中使用 MCP 刷新指令");
      return;
    }
    void store.materializeNewSessionDraft(draft_key);
  }

  return (
    <div className={styles.dock}>
      {store.interaction_error && (
        <div className={styles.error_notice} role="alert">
          <span>{store.interaction_error}</span>
          <button aria-label="关闭错误提示" onClick={() => store.clearInteractionError()} type="button"><Icon name="x" size={14} /></button>
        </div>
      )}
      <section className={styles.composer}>
        <SlashCommandMenu active_index={slash_active_index} items={slash_items} menu_ref={slash_ref} on_select={handleSlashCommand} open={slash_items.length > 0} />
        <SlashCommandHelp on_close={() => setShowHelp(false)} open={show_help} />
          <InputContextPicker
            label="技能"
            on_close={() => { setSkillPickerOpen(false); requestAnimationFrame(() => textarea_ref.current?.focus()); }}
            on_select={(skill) => {
              store.new_session_drafts.updateSelectedSkill(draft_key, skill.name);
              setSkillPickerOpen(false);
              requestAnimationFrame(() => textarea_ref.current?.focus());
            }}
            open={skill_picker_open}
            options={availableUserSkills(draft.skill_options)}
          />
        {mcp_picker_open && <McpServerPicker
          key={`${draft_key}:${draft.variant}`}
          on_close={() => { setMcpPickerOpen(false); requestAnimationFrame(() => textarea_ref.current?.focus()); }}
          on_select={(server) => { store.new_session_drafts.updateSelectedMcp(draft_key, server); setMcpPickerOpen(false); requestAnimationFrame(() => textarea_ref.current?.focus()); }}
          request={{ context: { type: "new_session", payload: { workspace_id: draft.workspace_id ?? undefined } }, variant: draft.variant }}
        />}
        <ComposerAttachmentContext
          attachments={attachment_flow.attachments}
          on_remove={attachment_flow.remove}
          owner_key={draft_key}
          paste_pending={attachment_flow.paste_pending}
          session_id={null}
        />
        {(draft.goal_armed || selected_skill || draft.selected_mcp) && (
          <div className={styles.draft_tags}>
            {draft.goal_armed && <span>
              目标
              <button aria-label="取消目标标记" onClick={() => store.new_session_drafts.updateGoalArmed(draft_key, false)} type="button"><Icon name="x" size={12} /></button>
            </span>}
            {selected_skill && <span title={selected_skill.name}>
              {selected_skill.name}
              <button aria-label={`移除技能 ${selected_skill.name}`} onClick={() => store.new_session_drafts.updateSelectedSkill(draft_key, null)} type="button"><Icon name="x" size={12} /></button>
            </span>}
            {draft.selected_mcp && <span title={draft.selected_mcp.server_key}>
              MCP · {draft.selected_mcp.display_name}
              <button aria-label={`移除 MCP ${draft.selected_mcp.display_name}`} onClick={() => store.new_session_drafts.updateSelectedMcp(draft_key, null)} type="button"><Icon name="x" size={12} /></button>
            </span>}
          </div>
        )}
        <textarea
          aria-label="输入消息"
          disabled={model_required || store.connection.state !== "connected" || store.composer_pending || attachment_flow.pending}
          onChange={(event) => store.new_session_drafts.updateText(draft_key, event.target.value)}
          onCompositionEnd={input_method.onCompositionEnd}
          onCompositionStart={input_method.onCompositionStart}
          onKeyDown={handleKeyDown}
          onKeyUp={input_method.onKeyUp}
          onPaste={(event) => handleComposerPaste(
            event,
            draft.text,
            (value) => store.new_session_drafts.updateText(draft_key, value),
            attachment_flow.pasteImages,
          )}
          placeholder={model_required ? "请先选择一个可用模型" : "输入消息…  / 使用指令"}
          ref={textarea_ref}
          rows={2}
          value={draft.text}
        />
        <footer className={styles.composer_actions}>
          <Tooltip content="添加附件">
            <Button aria-label="添加附件" disabled={store.composer_pending || attachment_flow.pending} iconOnly onClick={() => void attachment_flow.choose()} variant="outlined">
              <Icon name="plus" size={17} />
            </Button>
          </Tooltip>
          <ExecutionSettingsPopover
            approval_mode={draft.approval_mode}
            disabled={store.composer_pending}
            initial_category={null}
            on_approval_change={(mode) => {
              store.new_session_drafts.updateApprovalMode(draft_key, mode);
              return Promise.resolve(true);
            }}
            on_open_change={(open) => updateOverlay("execution", open)}
            on_variant_change={(variant) => {
              store.new_session_drafts.updateVariant(draft_key, variant);
              return Promise.resolve(true);
            }}
            open={active_overlay === "execution"}
            trigger_class_name={styles.execution_selector}
            variant={draft.variant}
          />
          <span className={styles.action_spacer} />
          <ModelSettingsPopover
            disabled={store.composer_pending}
            effort={draft.reasoning_effort}
            effort_options={[]}
            initial_category={null}
            model_display_name={draft.model_key ? modelDisplayName(application, draft.model_key) : "未选择模型"}
            model_key={draft.model_key}
            model_options={model_options}
            on_effort_change={(effort) => {
              store.new_session_drafts.updateReasoningEffort(draft_key, effort);
              return Promise.resolve(true);
            }}
            on_model_change={(model_key) => {
              store.new_session_drafts.updateModel(draft_key, model_key);
              return Promise.resolve(true);
            }}
            on_open_change={(open) => updateOverlay("model", open)}
            open={active_overlay === "model"}
            trigger_class_name={styles.model_selector}
          />
          <button
            aria-label="发送消息"
            className={styles.send_button}
            data-action="send"
            disabled={!can_send || model_required || store.composer_pending || attachment_flow.pending}
            onClick={submitNewDraft}
            type="button"
          >
            <Icon name="arrow-down" size={16} />
          </button>
        </footer>
      </section>
    </div>
  );
});

function ContextUsageRing({ view }: Readonly<{ view: SessionViewSnapshot }>) {
  const context = view.usage.context;
  const degrees = context ? Math.min(360, Math.max(0, context.usage_basis_points * 0.036)) : 0;
  const label = context ? `${formatCompact(context.used_tokens)} / ${formatCompact(context.window_tokens)} · ${(context.usage_basis_points / 100).toFixed(1)}%` : "暂无用量数据";
  return (
    <Tooltip content={label}>
      <span
        aria-label={`上下文用量：${label}`}
        className={styles.context_ring}
        role="img"
        style={{ "--context-degrees": `${degrees}deg` } as React.CSSProperties}
        tabIndex={0}
      />
    </Tooltip>
  );
}

function ComposerAttachmentContext(props: Readonly<{
  attachments: readonly ComposerAttachment[];
  on_remove: (attachment: ComposerAttachment) => void;
  paste_pending: boolean;
  session_id: SessionId | null;
  owner_key: string;
  quotes?: readonly QuotedTextSnapshot[];
  on_quote_remove?: (quote: QuotedTextSnapshot) => void;
  on_quote_locate?: (quote: QuotedTextSnapshot) => Promise<boolean>;
}>) {
  const [detail, setDetail] = useState<ComposerAttachment | null>(null);
  const [quote_detail, setQuoteDetail] = useState<QuotedTextSnapshot | null>(null);
  const [order, setOrder] = useState<readonly string[]>([]);
  const previous_owner_key = useRef(props.owner_key);
  const quotes = props.quotes ?? [];
  const context_items = [
    ...props.attachments.map((attachment) => ({ key: `attachment:${attachment.selection_id}`, type: "attachment" as const, attachment })),
    ...quotes.map((quote) => ({ key: `quote:${quote.quote_id}`, type: "quote" as const, quote })),
  ];
  useEffect(() => {
    const available = new Set(context_items.map((item) => item.key));
    const owner_changed = previous_owner_key.current !== props.owner_key;
    previous_owner_key.current = props.owner_key;
    if (owner_changed) {
      setDetail(null);
      setQuoteDetail(null);
    }
    setOrder((current) => [
      ...(owner_changed ? [] : current.filter((key) => available.has(key))),
      ...context_items.map((item) => item.key).filter((key) => owner_changed || !current.includes(key)),
    ]);
  }, [props.attachments, props.owner_key, props.quotes]);
  const by_key = new Map(context_items.map((item) => [item.key, item]));
  return (
    <>
      {(context_items.length > 0 || props.paste_pending) && (
        <div aria-label="待发送附件和引用" className={styles.attachment_list}>
          {order.flatMap((key) => {
            const item = by_key.get(key);
            if (!item) return [];
            if (item.type === "quote") return [(
              <div data-state="selected" key={item.key} title={item.quote.exact}>
                <button
                  aria-label={`查看引用 ${item.quote.exact}`}
                  className={styles.attachment_body}
                  onClick={() => setQuoteDetail(item.quote)}
                  type="button"
                >
                  <Icon name="quote" size={14} />
                  <span>{item.quote.exact}</span>
                  <small>{item.quote.source_label}</small>
                </button>
                <button
                  aria-label={`移除引用 ${item.quote.exact}`}
                  className={styles.attachment_remove}
                  onClick={() => {
                    if (quote_detail?.quote_id === item.quote.quote_id) setQuoteDetail(null);
                    props.on_quote_remove?.(item.quote);
                  }}
                  type="button"
                >
                  <Icon name="x" size={13} />
                </button>
              </div>
            )];
            const attachment = item.attachment;
            return [(
            <div data-state={attachment.state} key={item.key} title={attachment.error ?? attachment.original_name}>
              <button
                aria-label={`查看附件 ${attachment.original_name}`}
                className={styles.attachment_body}
                onClick={() => setDetail(attachment)}
                type="button"
              >
                <Icon name="paperclip" size={14} />
                <span>{attachment.original_name}</span>
                <small>{attachmentStateLabel(attachment)}</small>
              </button>
              <button
                aria-label={`移除附件 ${attachment.original_name}`}
                className={styles.attachment_remove}
                title={attachment.state === "uploading" ? "取消上传" : "移除附件"}
                onClick={() => {
                  if (detail?.selection_id === attachment.selection_id) setDetail(null);
                  props.on_remove(attachment);
                }}
                type="button"
              >
                <Icon name="x" size={13} />
              </button>
            </div>
            )];
          })}
          {props.paste_pending && <div className={styles.attachment_pending} role="status">正在添加图片…</div>}
        </div>
      )}
      <PresenceBoundary present={detail !== null}>
      {detail && (
        <AttachmentDetailDialog
          attachment={detail}
          on_close={() => setDetail(null)}
          session_id={props.session_id}
        />
      )}
      </PresenceBoundary>
      <PresenceBoundary present={quote_detail !== null}>
      {quote_detail && (
        <QuoteDetailDialog
          on_close={() => setQuoteDetail(null)}
          on_locate={() => props.on_quote_locate?.(quote_detail) ?? Promise.resolve(false)}
          quote={quote_detail}
        />
      )}
      </PresenceBoundary>
    </>
  );
}

function handleComposerPaste(
  event: React.ClipboardEvent<HTMLTextAreaElement>,
  current_value: string,
  set_value: (value: string) => void,
  paste_images: (files: readonly File[]) => Promise<boolean>,
) {
  const images = Array.from(event.clipboardData.items)
    .filter((item) => item.kind === "file" && item.type.toLocaleLowerCase().startsWith("image/"))
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
  if (images.length === 0) return;

  event.preventDefault();
  const pasted_text = event.clipboardData.getData("text/plain");
  if (pasted_text) {
    const target = event.currentTarget;
    const start = target.selectionStart ?? current_value.length;
    const end = target.selectionEnd ?? start;
    const next = `${current_value.slice(0, start)}${pasted_text}${current_value.slice(end)}`;
    const cursor = start + pasted_text.length;
    set_value(next);
    requestAnimationFrame(() => {
      target.focus();
      target.setSelectionRange(cursor, cursor);
    });
  }
  void paste_images(images);
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
  session_commands_available: boolean,
): string | null {
  if (command_name === "/mcp refresh") {
    if (!session_commands_available) return "当前 Runtime 不支持 MCP 刷新指令";
    if (view?.session.role === "controller") return "请在普通会话中刷新 MCP";
    return null;
  }
  if (command_name === "/skill") {
    if (!view || view.skill_catalog.status === "unavailable" || view.skill_catalog.status === "legacy_unavailable") {
      return "当前会话的技能信息不可用";
    }
    if (availableUserSkills(view.skill_catalog.skills).length === 0) return "当前会话没有用户可选技能";
    return null;
  }
  if (command_name === "/title" && view?.session.role === "controller") return "主控标题固定";
  if (command_name !== "/goal") return null;
  if (view?.goal) return "当前会话已有目标，请先继续或退出现有目标";
  return null;
}

function draftSlashDisabledReason(command_name: string, draft: NewSessionDraft | null): string | null {
  if (command_name === "/compact" || command_name === "/title" || command_name === "/mcp refresh") return "发送第一条消息后可用";
  if (command_name === "/skill") {
    if (!draft || draft.skill_status === "failed") return "当前工作空间的技能信息不可用";
    if (draft.skill_status !== "ready") return "正在读取当前工作空间的技能";
    if (availableUserSkills(draft.skill_options).length === 0) return "当前工作空间没有用户可选技能";
  }
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
