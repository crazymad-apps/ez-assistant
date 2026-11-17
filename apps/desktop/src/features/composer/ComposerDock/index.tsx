import { AppComposerDraftReceiver } from '../../mcp-apps/AppComposerDraftReceiver';
import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState } from 'react';
import type {
  ApprovalDecision,
  CustomAgentSelection,
  CustomAgentSnapshot,
  McpSelectionTagSnapshot,
  PreparedCustomAgentSnapshot,
  QuotedTextSnapshot,
  SessionId,
  SessionViewSnapshot,
  SkillSummarySnapshot,
} from '@ez-assistant/protocol';
import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { PresenceBoundary } from '../../../components/Presence';
import { useInputMethodGuard } from '../../../components/InputMethodGuard';
import { Tooltip } from '../../../components/Tooltip';
import { Dialog } from '../../../components/Dialog';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '../../../components/DropdownMenu';
import { ComposerSecondaryDrawer } from './ComposerSecondaryDrawer';
import { draftKeyForWorkspace, type NewSessionDraft, type NewSessionDraftKey } from '../../../stores/NewSessionDraftStore';
import { useRootStore } from '../../../stores/RootStoreContext';
import type { HistoryEditDraft, HistoryEditStore } from '../../../stores/HistoryEditStore';
import { FeedbackPanel } from '../../feedback/FeedbackPanel';
import { modelSelectionIssue, modelSelectionLabel } from '../../settings';
import { ApprovalWorkspace, isAllowDecision } from './ApprovalWorkspace';
import { ComposerNotice } from './ComposerNotice';
import { AttachmentDetailDialog } from './AttachmentDetailDialog';
import { QuoteDetailDialog } from '../../conversation/QuoteDetailDialog';
import { formatCompact, SLASH_COMMANDS, type SlashCommandItem } from './composerOptions';
import { ExecutionSettingsPopover } from './ExecutionSettingsPopover';
import { GoalStatusRow } from './GoalStatusRow';
import { ModelSettingsPopover } from './ModelSettingsPopover';
import { useDraftModelEfforts } from './useDraftModelEfforts';
import { OutputHostingMenu } from './OutputHostingMenu';
import { QueueDrawer } from './QueueDrawer';
import { queuePresentation } from './queuePresentation';
import { parseSessionCommand } from './sessionCommand';
import { SlashCommandHelp, SlashCommandMenu } from './SlashCommandMenu';
import { SkillPicker } from './SkillPicker';
import { McpServerPicker } from './McpServerPicker';
import { DevicePicker } from './DevicePicker';
import { WorkspacePicker } from './WorkspacePicker';
import { ContextPicker } from './ContextPicker';
import { PluginOperationDialog } from '../../settings/SettingsDialog/PluginOperationDialog';
import { TodoSummary } from './TodoSummary';
import { type ComposerAttachment, useComposerAttachments } from './useComposerAttachments';
import { QUICK_ACTIONS, resolveQuickAction, type QuickActionConfig, type ResolvedQuickAction } from './quickActions';
import styles from './index.module.scss';

export const ComposerDock = observer(function ComposerDock({
  read_only = false,
}: Readonly<{
  read_only?: boolean;
}>) {
  const store = useRootStore();
  const draft_key = store.navigation.selected_draft_key;
  if (draft_key) {
    return read_only ? null : <NewSessionDraftComposer draft_key={draft_key} />;
  }
  return <SessionComposerDock read_only={read_only} />;
});

const SessionComposerDock = observer(function SessionComposerDock({
  read_only = false,
}: Readonly<{
  read_only?: boolean;
}>) {
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const session = session_view?.session;
  const receipt_run = session_id ? store.projection.input_receipts.get(session_id)?.at(-1)?.run : undefined;
  const active_run_id = session ? store.live_execution.activeRunId(session.session_id, receipt_run?.run_id ?? session.active_run_id) : null;
  const feedback = session_id ? store.feedback.waiting(session_id) : undefined;
  const editor = store.history_edit;
  const ordinary_key = editor.ordinaryKey;
  const ordinary = editor.ordinary(ordinary_key);
  const edit = read_only ? undefined : editor.draft;
  const edit_key = edit?.key;
  const edit_locked = Boolean(edit && edit.phase !== 'editing');
  const edit_stale = Boolean(edit && editor.stale);
  const edit_busy = Boolean(edit && editor.sessionBusy);
  const accepted = editor.accepted;
  const draft = edit?.text ?? ordinary.text;
  const variant = edit?.variant ?? session?.current_variant;
  const goal_armed = !edit && ordinary.goal_armed;
  const selected_skill = !edit ? ordinary.skill : historySkillTag(edit);
  const selected_mcp = edit ? edit.mcp : ordinary.mcp;
  const expanded_drawer = ordinary.drawer;

  function setDraft(text: string) {
    if (edit_key) editor.update(ordinary_key, edit_key, { text });
    else editor.updateOrdinary(ordinary_key, { text });
  }

  function setGoalArmed(value: boolean) {
    editor.updateOrdinary(ordinary_key, { goal_armed: value });
  }

  function setSelectedSkill(skill: SkillSummarySnapshot | null) {
    if (edit_key) editor.update(ordinary_key, edit_key, { skill_edit: skill ? { type: 'select', name: skill.name } : { type: 'clear' } });
    else editor.updateOrdinary(ordinary_key, { skill });
  }

  function setSelectedMcp(mcp: McpSelectionTagSnapshot | null) {
    if (edit_key) editor.update(ordinary_key, edit_key, { mcp, mcp_edit: mcp ? { type: 'select', server_key: mcp.server_key } : { type: 'clear' } });
    else editor.updateOrdinary(ordinary_key, { mcp });
  }

  function setExpandedDrawer(drawer: typeof expanded_drawer) {
    editor.updateOrdinary(ordinary_key, { drawer });
  }

  const [skill_picker_open, setSkillPickerOpen] = useState(false);
  const [mcp_picker_open, setMcpPickerOpen] = useState(false);
  const [active_overlay, setActiveOverlay] = useState<'todo' | 'execution' | 'model' | null>(null);
  const [initial_settings_category, setInitialSettingsCategory] = useState<
    'variant' | 'approval' | 'model' | 'effort' | null
  >(null);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const [approval_minimized, setApprovalMinimized] = useState(false);
  const [approval_decision, setApprovalDecision] = useState<ApprovalDecision | null>(null);
  const [show_help, setShowHelp] = useState(false);
  const textarea_ref = useRef<HTMLTextAreaElement>(null);
  const slash_ref = useRef<HTMLDivElement>(null);
  const input_method = useInputMethodGuard();
  const previous_approval_count = useRef(0);
  const previous_approval_session_id = useRef<string | null>(null);
  const input_owner = useRef({ key: ordinary_key, edit_key, variant });
  // 上传与提交回调只可更新原草稿，切换视图不能清掉新草稿。
  if (input_owner.current.key !== ordinary_key || input_owner.current.edit_key !== edit_key || input_owner.current.variant !== variant) {
    input_owner.current = { key: ordinary_key, edit_key, variant };
  }
  const attachment_flow = useComposerAttachments({
    attachments: edit?.attachments ?? ordinary.attachments,
    disabled: edit_locked || store.composer_pending || store.pending_compaction_session_id === session_id,
    on_attachments_change: (attachments) => {
      if (edit_key) editor.updateAttachments(ordinary_key, edit_key, attachments);
      else editor.updateOrdinary(ordinary_key, { attachments });
    },
    on_error: (message) => store.showInteractionError(message),
    owner_key: edit_key ?? ordinary_key,
  });
  const quotes = edit?.quotes ?? store.composer_quotes.get(session_id);

  const slash_query = !edit && draft.startsWith('/') && !draft.includes('\n') ? draft.toLocaleLowerCase() : null;
  const slash_items: readonly SlashCommandItem[] =
    slash_query === null
      ? []
      : SLASH_COMMANDS.filter(
          (item) =>
            item.name.includes(slash_query) || item.description.toLocaleLowerCase().includes(slash_query.slice(1)),
        ).map((item) => ({
          ...item,
          disabled_reason: slashDisabledReason(
            item.name,
            session_view,
            application?.capabilities.session_commands ?? false,
          ),
        }));

  useEffect(() => {
    const first_enabled = slash_items.findIndex((item) => !item.disabled_reason);
    const exact_index = slash_items.findIndex((item) => item.name === slash_query);
    setSlashActiveIndex(exact_index >= 0 ? exact_index : Math.max(0, first_enabled));
  }, [slash_query]);

  useEffect(() => {
    setSkillPickerOpen(false);
    setMcpPickerOpen(false);
    setActiveOverlay(current => current === 'todo' ? current : null);
    setShowHelp(false);
  }, [ordinary_key, edit_key]);

  const ordinary_variant = useRef({ key: ordinary_key, variant: session?.current_variant });
  useEffect(() => {
    const previous = ordinary_variant.current;
    if (previous.key === ordinary_key && previous.variant !== session?.current_variant) {
      editor.updateOrdinary(ordinary_key, { mcp: null });
    }
    ordinary_variant.current = { key: ordinary_key, variant: session?.current_variant };
    setMcpPickerOpen(false);
  }, [editor, ordinary_key, session?.current_variant]);

  const client = store.runtime_client;
  const connected = store.connection.state === 'connected' && !store.projection.is_stale;
  const supports_intervention = Boolean(application?.capabilities.input_intervention);
  const intervention = store.intervention;
  const draft_intervening = intervention?.target.type === 'draft';
  useEffect(() => {
    if (!read_only && session_id && connected && intervention?.phase === 'uncertain') {
      void store.confirmIntervention(session_id, true);
    }
  }, [session_id, connected, intervention, client, read_only, store]);
  const edit_mcp_key = edit?.mcp?.server_key;
  const mcp_scope = edit_key && edit_mcp_key && variant
    ? JSON.stringify([edit_key, edit_mcp_key, variant, store.projection.card_sources_changed])
    : null;
  const [mcp_check, setMcpCheck] = useState<{ scope: string; issue: string | null } | null>(null);
  const mcp_check_pending = Boolean(mcp_scope && mcp_check?.scope !== mcp_scope);
  const mcp_issue = mcp_scope && mcp_check?.scope === mcp_scope ? mcp_check.issue : null;
  useEffect(() => {
    if (!mcp_scope || !edit_mcp_key || !session_id || !variant || !connected) return;
    let current = true;
    void store.listMcpServerOptions({ context: { type: 'session', payload: { session_id } }, variant })
      .then(servers => {
        if (!current) return;
        const available = servers.some(server => server.server_key === edit_mcp_key);
        setMcpCheck({ scope: mcp_scope, issue: available ? null : '原 MCP 服务当前不可用，请移除或替换。' });
      })
      .catch(() => {
        if (current) setMcpCheck({ scope: mcp_scope, issue: '无法核对 MCP 服务，请重新选择或移除。' });
      });
    return () => { current = false; };
  }, [client, connected, edit_mcp_key, mcp_scope, session_id, store, variant]);
  useEffect(() => {
    if (connected && edit?.phase === 'uncertain') void editor.retry(true);
  }, [client, connected, edit?.phase, editor]);
  const accepted_result = accepted?.result;
  useEffect(() => {
    if (connected && accepted_result && !read_only) void editor.refresh(true);
  }, [accepted_result, connected, editor, ordinary_key, read_only]);

  useEffect(() => {
    const count = session_view?.approvals.items.length ?? 0;
    if (count > 0 && (previous_approval_count.current === 0 || previous_approval_session_id.current !== session_id)) {
      setApprovalMinimized(false);
    }
    previous_approval_count.current = count;
    previous_approval_session_id.current = session_id;
  }, [session_view?.approvals.items.length, session_id]);

  const notification_target = store.notifications?.focus_target;
  const [focused_approval_id, setFocusedApprovalId] = useState<string | null>(null);
  useEffect(() => {
    if (notification_target?.kind !== "approval" || notification_target.owner.session_id !== session_id) return;
    setApprovalMinimized(false);
    setFocusedApprovalId(notification_target.approval_id);
    store.notifications.consumeFocus(notification_target);
  }, [notification_target, session_id, store.notifications]);
  const approval = session_view?.approvals.items.find((item) => item.approval_id === focused_approval_id)
    ?? session_view?.approvals.items[0];
  useEffect(() => {
    if (!approval) {
      setApprovalDecision(null);
      return;
    }
    setApprovalDecision(approval.available_decisions.find(isAllowDecision) ?? null);
  }, [approval?.approval_id]);

  useEffect(() => {
    if ((approval && !approval_minimized) || feedback) {
      setActiveOverlay(null);
      setShowHelp(false);
    }
    if (feedback) {
      setSkillPickerOpen(false);
      setMcpPickerOpen(false);
    }
  }, [approval, approval_minimized, feedback?.request_id]);

  useEffect(() => {
    resizeTextarea(textarea_ref.current);
  }, [draft, feedback?.request_id]);

  const focus_revision = editor.focus_revision;
  const applied_focus = useRef({ revision: focus_revision, edit_key: undefined as string | undefined });
  useEffect(() => {
    const textarea = textarea_ref.current;
    if (!textarea || read_only || feedback || (approval && !approval_minimized)) return;
    if (applied_focus.current.revision !== focus_revision || (edit_key && applied_focus.current.edit_key !== edit_key)) {
      textarea.focus({ preventScroll: true });
      textarea.setSelectionRange(textarea.value.length, textarea.value.length);
    }
    applied_focus.current = { revision: focus_revision, edit_key };
  }, [focus_revision, edit_key, read_only, feedback, approval, approval_minimized]);

  if (!session || !session_view) {
    return null;
  }
  if (read_only && !approval) {
    return null;
  }

  const is_archived = session.lifecycle === 'archived';
  const is_idle_for_model = !active_run_id && session_view.queue.items.length === 0;
  const selected_model = session_view.composer_capabilities.selected_model ?? null;
  const model_error = session_view.composer_capabilities.model_error;
  const model_required = selected_model === null || Boolean(model_error);
  const is_control_command = parseSessionCommand(draft).type !== 'not_command';
  const queue_presentation = queuePresentation(session_view.queue, active_run_id);
  const manual_compaction = session.active_compaction?.trigger.type === 'manual' ? session.active_compaction : null;
  const compaction_command_pending = store.pending_compaction_session_id === session.session_id;
  const unavailable_attachment = Boolean(edit?.attachments.some(item => item.attachment_id && item.state === 'failed'));
  const edit_can_send = Boolean(edit && connected && application?.capabilities.user_message_edit
    && !edit_locked && !edit_stale && !edit_busy && !editor.busyTarget && !unavailable_attachment && !mcp_check_pending && !mcp_issue
    && !attachment_flow.pending && !model_required
    && (draft.trim() || attachment_flow.attachments.length || quotes.length));
  // 窗口占用由 Runtime 根据当前有效上下文计算，不能用旧 step 或累计用量取最大值。
  const context_usage = session_view?.usage.context ?? null;

  async function submitCompaction() {
    if (draft.trim() !== '/compact') {
      return;
    }
    setDraft('');
    if (attachment_flow.attachments.length > 0 || quotes.length > 0 || goal_armed || selected_skill || selected_mcp) {
      store.showInteractionError('/compact 必须单独使用，请先移除附件、引用和标签。');
      return;
    }
    if (
      active_run_id ||
      session!.queued_input_count > 0 ||
      session!.pending_approval_count > 0 ||
      session!.active_child_count > 0 ||
      session!.active_compaction ||
      compaction_command_pending ||
      store.pending_session_action
    ) {
      store.showInteractionError('当前会话正忙，暂时不能压缩上下文。');
      return;
    }
    await store.compactSession(session!.session_id, session_view!.conversation_generation);
  }

  async function submitDraft(value = draft, intervene = false) {
    if (edit) {
      if (edit_can_send) await editor.submit(() => attachment_flow.uploadAll(session!.session_id));
      return;
    }
    if (!intervene && session_id && store.feedback.waiting(session_id)) {
      store.showInteractionError('请先回复问题，或取消等待后发送新指令。');
      return;
    }
    const owner = input_owner.current;
    const control = parseSessionCommand(value);
    if (store.intervention || (intervene && (!supports_intervention || !connected || is_archived
      || goal_armed || slash_query !== null || control.type !== 'not_command' || manual_compaction || compaction_command_pending))) return;
    if (control.type === 'invalid') {
      store.showInteractionError(control.message);
      return;
    }
    if (control.type === 'command') {
      if (attachment_flow.attachments.length > 0 || quotes.length > 0 || selected_skill || goal_armed || selected_mcp) {
        store.showInteractionError('刷新指令不能同时携带附件、引用或标签，请先移除这些内容');
        return;
      }
      if (await store.submitSessionCommand(session!.session_id, control.command)) setDraft('');
      return;
    }
    if (value.trim() === '/compact') {
      await submitCompaction();
      return;
    }
    if (value.trim() === '/title') {
      setDraft('');
      await store.generateSessionTitle(session!.session_id);
      return;
    }
    if (
      (!value.trim() && attachment_flow.attachments.length === 0 && quotes.length === 0) ||
      model_required ||
      store.composer_pending ||
      attachment_flow.pending
    ) {
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
    if (!attachment_ids || owner !== input_owner.current || editor.ordinaryKey !== ordinary_key) {
      return;
    }
    const mode = goal_armed ? 'start_goal' : 'normal';
    const uploaded = editor.ordinary(ordinary_key).attachments;
    const quote_store = store.composer_quotes;
    const clearInterventionDraft = () => {
      const current = editor.ordinary(ordinary_key);
      // 回执核实可能发生在切回会话后，只清理原草稿中已提交的内容。
      if (current.text === value && current.attachments === uploaded
        && current.skill === selected_skill && current.mcp === selected_mcp && !current.goal_armed) {
        editor.updateOrdinary(ordinary_key, { text: '', attachments: [], skill: null, mcp: null });
        for (const quote of quotes) quote_store.remove(session!.session_id, quote.quote_id);
      }
    };
    const args = [
      session!.session_id,
      value,
      session!.current_variant,
      attachment_ids,
      mode,
      selected_skill?.name ?? null,
      quotes,
      selected_mcp?.server_key ?? null,
    ] as const;
    if (intervene) {
      await store.submitInput(...args, 'intervention', clearInterventionDraft);
      return;
    }
    const submitted = await store.submitInput(...args);
    if (submitted && owner === input_owner.current) {
      setDraft('');
      setGoalArmed(false);
      setSelectedSkill(null);
      setSelectedMcp(null);
      attachment_flow.clear();
      store.composer_quotes.clear(session!.session_id);
    }
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.name === '/mcp refresh' || command.name === '/skill refresh') {
      if (command.disabled_reason) {
        store.showInteractionError(command.disabled_reason);
      } else {
        setDraft(command.name);
        void submitDraft(command.name);
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
    if (command.name === '/compact') {
      if (draft.trim() === '/compact') {
        void submitCompaction();
      } else {
        setDraft('/compact');
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
    if (command.name === '/title') {
      if (command.disabled_reason) {
        setDraft('');
        store.showInteractionError(command.disabled_reason);
      } else if (draft.trim() === '/title') {
        setDraft('');
        void store.generateSessionTitle(session!.session_id);
      } else {
        setDraft('/title');
        requestAnimationFrame(() => textarea_ref.current?.focus());
      }
      return;
    }
    if (command.disabled_reason) {
      setDraft('');
      store.showInteractionError(command.disabled_reason);
      requestAnimationFrame(() => textarea_ref.current?.focus());
      return;
    }
    if (command.name === '/goal') {
      setDraft('');
      setGoalArmed(true);
      requestAnimationFrame(() => textarea_ref.current?.focus());
      return;
    }
    if (command.name === '/skill') {
      setDraft('');
      setSkillPickerOpen(true);
      return;
    }
    if (command.name === '/mcp') {
      setDraft('');
      setSkillPickerOpen(false);
      setMcpPickerOpen(true);
      return;
    }
    if (command.picker) {
      setDraft('');
      setInitialSettingsCategory(command.picker);
      setActiveOverlay(command.picker === 'model' ? 'model' : 'execution');
      return;
    }
    if (command.name === '/new') {
      setDraft('');
      store.openNewSessionDraft(session!.workspace_id, store.navigation.target_device);
      return;
    }
    setDraft('');
    setShowHelp(true);
  }

  function handleTextareaKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) {
      return;
    }
    if (edit) {
      if (event.key === 'Enter' && !event.shiftKey) {
        event.preventDefault();
        if (!event.ctrlKey && !event.metaKey) void submitDraft();
      }
      return;
    }
    if (slash_items.length > 0 && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault();
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      const next = nextEnabledSlashIndex(slash_items, slash_active_index, direction);
      setSlashActiveIndex(next);
      slash_ref.current
        ?.querySelector<HTMLElement>(`[data-slash-index="${next}"]`)
        ?.scrollIntoView({ block: 'nearest' });
      return;
    }
    if (event.key === 'Escape' && slash_query !== null) {
      event.preventDefault();
      setDraft(draft.slice(1));
      return;
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      if (event.ctrlKey || event.metaKey) {
        if (isInterventionShortcut(event)) void submitDraft(draft, true);
        return;
      }
      const selected_command = slash_query === null ? undefined : slash_items[slash_active_index];
      if (selected_command) {
        handleSlashCommand(selected_command);
        return;
      }
      const exact_command = SLASH_COMMANDS.find(
        (item) => item.name === event.currentTarget.value.trim().toLocaleLowerCase(),
      );
      const exact = exact_command
        ? {
            ...exact_command,
            disabled_reason: slashDisabledReason(
              exact_command.name,
              session_view,
              application?.capabilities.session_commands ?? false,
            ),
          }
        : undefined;
      if (exact) {
        handleSlashCommand(exact);
        return;
      }
      void submitDraft();
    }
  }

  function handleDraftChange(value: string) {
    setDraft(value);
    if (!edit && value.startsWith('/') && !value.includes('\n')) {
      setActiveOverlay(null);
      setShowHelp(false);
    }
  }

  const goal = session_view.goal;
  const compaction_cancels_from_primary = !edit && Boolean(manual_compaction?.cancellable);
  const has_composer_content = Boolean(draft.trim() || attachment_flow.attachments.length || quotes.length);
  const goal_stops_from_primary =
    !edit && !compaction_cancels_from_primary && !has_composer_content && goal?.state === 'running';
  const run_interrupts_from_primary =
    !edit && !has_composer_content && !goal_stops_from_primary && Boolean(active_run_id);
  const primary_action_available =
    compaction_cancels_from_primary || has_composer_content || goal_stops_from_primary || run_interrupts_from_primary;
  const primary_action = resolvePrimaryAction(
    compaction_cancels_from_primary,
    goal_stops_from_primary,
    run_interrupts_from_primary,
  );
  const primary_label = primaryActionLabel(primary_action);
  const normal_send_label = has_composer_content && !compaction_cancels_from_primary ? '发送消息' : primary_label;
  const show_send_shortcut = supports_intervention && !goal_armed && !is_control_command && slash_query === null
    && primary_action === 'send';
  let placeholder = '输入消息…  / 使用指令';
  if (edit) placeholder = '编辑历史消息…';
  else if (model_required) placeholder = '选择模型后开始对话，或输入 / 使用控制指令';
  else if (manual_compaction || compaction_command_pending) placeholder = '正在压缩上下文…';
  else if (feedback) placeholder = `输入新指令，${interventionShortcutLabel()} 介入…`;
  else if (goal?.state === 'paused') placeholder = '输入新指导并恢复目标…';
  const execution_initial_category =
    active_overlay === 'execution' &&
    (initial_settings_category === 'variant' || initial_settings_category === 'approval')
      ? initial_settings_category
      : null;
  const model_initial_category =
    active_overlay === 'model' && (initial_settings_category === 'model' || initial_settings_category === 'effort')
      ? initial_settings_category
      : null;

  function updateActiveOverlay(overlay: Exclude<typeof active_overlay, null>, open: boolean) {
    setActiveOverlay((current) => {
      if (open) return overlay;
      return current === overlay ? null : current;
    });
  }

  if (!read_only && feedback && !supports_intervention) {
    return <div className={styles.dock}>
      <FeedbackPanel key={feedback.request_id} request={feedback} store={store.feedback} />
    </div>;
  }

  return (
    <div className={styles.dock}>
      <div className={styles.composer_top}>
        {!read_only && feedback && <FeedbackPanel key={feedback.request_id} request={feedback} store={store.feedback} />}
        {!read_only && session_id && store.feedback.errors.has(session_id) && <ComposerNotice tone="error"
          action={{ label: '重试', onClick: () => void store.feedback.refresh(session_id) }}>
          无法读取问答状态：{store.feedback.errors.get(session_id)}
        </ComposerNotice>}
        {!edit && !read_only && session_view.work_plan && session_view.work_plan.items.length > 0 && (
          <TodoSummary
            on_open_change={(open) => updateActiveOverlay('todo', open)}
            open={active_overlay === 'todo'}
            running={Boolean(active_run_id)}
            work_plan={session_view.work_plan}
          />
        )}
        {model_error && (
          <ComposerNotice
            tone="error"
            action={{
              label: '配置模型',
              onClick: () => store.settings.open('models'),
            }}
          >
            {model_error.message}
          </ComposerNotice>
        )}
        {store.interaction_error && (
          <ComposerNotice tone="error" dismiss={{ label: '关闭错误提示', onClick: () => store.clearInteractionError() }}>
            {store.interaction_error}
          </ComposerNotice>
        )}
        {store.session_notice?.session_id === session.session_id && (
          <ComposerNotice
            tone={store.session_notice.tone}
            action={
              store.session_notice.action === 'retry_title'
                ? { label: '重试', onClick: () => void store.generateSessionTitle(session.session_id) }
                : undefined
            }
            dismiss={{ label: '关闭状态提示', onClick: () => store.clearSessionNotice(session.session_id) }}
          >
            {store.session_notice.message}
          </ComposerNotice>
        )}
        {!edit && !read_only && session_view.title_generation?.trigger === 'manual' && (
          <div className={styles.compaction_status} role="status">
            <span className={styles.loading_ring} />
            <strong>正在生成标题</strong>
          </div>
        )}
        {!edit && !read_only && goal && (
          <GoalStatusRow
            goal={goal}
            on_clear={() => store.clearGoal(session.session_id, goal.goal_id, goal.generation)}
            on_open_change={(open) => setExpandedDrawer(open ? 'goal' : null)}
            on_resume={() => store.resumeGoal(session.session_id, goal.goal_id, goal.generation)}
            on_stop={() => store.stopGoal(session.session_id, goal.goal_id, goal.generation)}
            open={expanded_drawer === 'goal'}
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
        {!edit && !read_only && queue_presentation.visible && (
          <QueueDrawer
            goal={goal}
            intervention_available={supports_intervention && !is_archived}
            intervention_blocked={Boolean(session.active_compaction) || compaction_command_pending}
            open={expanded_drawer === 'queue'}
            on_open_change={(open) => setExpandedDrawer(open ? 'queue' : null)}
            queue={session_view.queue}
            presentation={queue_presentation}
            session_id={session.session_id}
          />
        )}
        {!read_only && intervention && <ComposerNotice tone="neutral"
          action={intervention.phase === 'uncertain' && connected ? {
            label: '核实', onClick: () => void store.confirmIntervention(session.session_id),
          } : undefined}>
          {intervention.phase === 'submitting' && connected ? '正在介入…' : '正在确认…'}
        </ComposerNotice>}
        {!edit && !read_only && session.active_compaction && (
          <div className={styles.compaction_status} role="status">
            <span className={styles.loading_ring} />
            <strong>{compactionStatusLabel(session.active_compaction.trigger)}</strong>
          </div>
        )}
        {!read_only && accepted && <ComposerNotice tone={accepted.busy ? 'neutral' : 'error'}
          action={!accepted.busy ? { label: '重新读取', onClick: () => void editor.refresh() } : undefined}>
          {accepted.busy ? '修改已发送，正在刷新历史…' : '修改已发送，消息刷新失败。'}
        </ComposerNotice>}
        {edit && (edit.phase === 'submitting' || edit.phase === 'uncertain' || edit.error || edit_stale || edit_busy) && (
          <HistoryEditNotice editor={editor} connected={connected} />
        )}
        {edit && !edit_locked && !edit_busy && mcp_issue && <ComposerNotice tone="warning">{mcp_issue}</ComposerNotice>}
        {edit && (!approval || approval_minimized) && (
          <ComposerSecondaryDrawer
            label="正在编辑历史消息"
            summary={<strong>正在编辑历史消息</strong>}
            actions={<>
              <Button variant="text" size="small" disabled={edit.locating || !connected}
                onClick={() => void editor.locateOriginal()}>{edit.locating ? '正在定位…' : '返回原位置'}</Button>
              <Button variant="text" size="small" disabled={edit_locked} onClick={() => editor.cancel()}>取消</Button>
            </>}
          />
        )}
      </div>
      {approval && !approval_minimized ? (
        <ApprovalWorkspace
          approval={approval}
          child_title={
            approval.child_task_id
              ? (session_view.child_tasks.find((item) => item.task.child_task_id === approval.child_task_id)?.task
                  .title ?? null)
              : null
          }
          decision={approval_decision}
          on_decision_change={setApprovalDecision}
          on_minimize={() => setApprovalMinimized(true)}
          queue_revision={session_view.queue.revision}
          remaining={session_view.approvals.items.length - 1}
        />
      ) : read_only ? null : is_archived && !edit ? (
        <div className={styles.archived_notice}>
          <span>此会话已归档，只能查看历史内容。</span>
          <button
            disabled={store.composer_pending}
            onClick={() => void store.restoreSession(session.session_id)}
            type="button"
          >
            恢复会话
          </button>
        </div>
      ) : (
        <section className={styles.composer}>
          {!edit && session.proxy && (
            <div className={styles.proxy_takeover_hint}>当前由主控代理；发送消息后将由你接管并退出代理。</div>
          )}
          <SlashCommandMenu
            active_index={slash_active_index}
            items={slash_items}
            menu_ref={slash_ref}
            on_select={handleSlashCommand}
            open={slash_items.length > 0}
          />
          <SlashCommandHelp on_close={() => setShowHelp(false)} open={show_help} />
          {skill_picker_open && (
            <SkillPicker
              key={edit_key ?? session.session_id}
              workspace_id={session.workspace_id}
              on_close={() => {
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              on_select={(skill) => {
                setSelectedSkill(skill);
                setSkillPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
            />
          )}
          {mcp_picker_open && (
            <McpServerPicker
              key={`${edit_key ?? session.session_id}:${variant}`}
              on_close={() => {
                setMcpPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              on_select={(server) => {
                setSelectedMcp(server);
                setMcpPickerOpen(false);
                requestAnimationFrame(() => textarea_ref.current?.focus());
              }}
              request={{
                context: { type: 'session', payload: { session_id: session.session_id } },
                variant: variant ?? session.current_variant,
              }}
            />
          )}
          <ComposerAttachmentContext
            attachments={attachment_flow.attachments}
            on_quote_locate={(quote) => store.locateTextQuoteSource(session.session_id, quote)}
            on_quote_remove={(quote) => {
              if (edit_key) editor.update(ordinary_key, edit_key, { quotes: quotes.filter(item => item.quote_id !== quote.quote_id) });
              else store.composer_quotes.remove(session.session_id, quote.quote_id);
            }}
            on_remove={attachment_flow.remove}
            remove_disabled={edit_locked || draft_intervening}
            owner_key={edit_key ?? ordinary_key}
            paste_pending={attachment_flow.paste_pending}
            quotes={quotes}
            session_id={session.session_id}
          />
          {(goal_armed || selected_skill || selected_mcp) && (
            <div className={styles.draft_tags}>
              {goal_armed && (
                <span>
                  目标
                  <button aria-label="取消目标标记" onClick={() => setGoalArmed(false)} type="button">
                    <Icon name="x" size={12} />
                  </button>
                </span>
              )}
              {selected_skill && (
                <span title={selected_skill.name}>
                  {selected_skill.name}
                  <button
                    aria-label={`移除技能 ${selected_skill.name}`}
                    disabled={edit_locked || draft_intervening}
                    onClick={() => setSelectedSkill(null)}
                    type="button"
                  >
                    <Icon name="x" size={12} />
                  </button>
                </span>
              )}
              {selected_mcp && (
                <span data-unavailable={mcp_issue ? true : undefined} title={mcp_issue ?? selected_mcp.server_key}>
                  MCP · {selected_mcp.display_name}
                  <button
                    aria-label={`移除 MCP ${selected_mcp.display_name}`}
                    disabled={edit_locked || draft_intervening}
                    onClick={() => setSelectedMcp(null)}
                    type="button"
                  >
                    <Icon name="x" size={12} />
                  </button>
                </span>
              )}
            </div>
          )}
          {!edit && !draft_intervening && <AppComposerDraftReceiver text={draft} has_other_content={has_composer_content} on_fill={setDraft} />}
          <textarea
            aria-label={edit ? '编辑消息' : '输入消息'}
            readOnly={edit_locked || draft_intervening}
            disabled={
              !edit && (store.connection.state !== 'connected' ||
              store.composer_pending ||
              attachment_flow.pending ||
              Boolean(manual_compaction) ||
              compaction_command_pending)
            }
            onChange={(event) => handleDraftChange(event.target.value)}
            onCompositionEnd={input_method.onCompositionEnd}
            onCompositionStart={input_method.onCompositionStart}
            onKeyDown={handleTextareaKeyDown}
            onKeyUp={input_method.onKeyUp}
            onPaste={(event) => {
              if (!edit_locked && !draft_intervening) handleComposerPaste(event, draft, setDraft, attachment_flow.pasteImages);
            }}
            placeholder={placeholder}
            ref={textarea_ref}
            rows={2}
            value={draft}
          />
          <footer className={styles.composer_actions}>
            {edit ? <EditContextMenu disabled={edit_locked || attachment_flow.pending || !connected}
              on_attachment={() => void attachment_flow.choose()}
              on_skill={() => { setMcpPickerOpen(false); setSkillPickerOpen(true); }}
              on_mcp={() => { setSkillPickerOpen(false); setMcpPickerOpen(true); }}
            /> : <Tooltip content="添加附件">
              <Button
                aria-label="添加附件"
                className={styles.attachment_button}
                disabled={
                  store.composer_pending ||
                  draft_intervening ||
                  attachment_flow.pending ||
                  Boolean(manual_compaction) ||
                  compaction_command_pending
                }
                iconOnly
                onClick={() => void attachment_flow.choose()}
                variant="text"
              >
                <Icon name="plus" size={17} />
              </Button>
            </Tooltip>}
            <ExecutionSettingsPopover
              approval_mode={session.approval_mode}
              disabled={edit_locked || store.composer_pending || Boolean(manual_compaction) || compaction_command_pending}
              initial_category={execution_initial_category}
              on_approval_change={(mode) => store.setSessionApprovalMode(session.session_id, mode)}
              on_open_change={(open) => {
                setShowHelp(false);
                if (open) setInitialSettingsCategory(null);
                updateActiveOverlay('execution', open);
              }}
              on_variant_change={async (next_variant) => {
                if (!edit_key) return store.setSessionVariant(session.session_id, next_variant);
                editor.update(ordinary_key, edit_key, { variant: next_variant });
                return true;
              }}
              open={active_overlay === 'execution'}
              trigger_class_name={styles.execution_selector}
              variant={variant ?? session.current_variant}
            />
            {!edit && session.role === 'controller' && <OutputHostingMenu session={session} />}
            <span className={styles.action_spacer} />
            <ContextUsageRing context={context_usage} />
            <ModelSettingsPopover
              disabled={edit_locked || store.composer_pending || Boolean(manual_compaction) || compaction_command_pending}
              effort={session.reasoning_effort ?? null}
              effort_options={session_view.composer_capabilities.reasoning_effort_options}
              initial_category={model_initial_category}
              model_display_name={composerModelLabel(
                application, session.model_selection, selected_model,
                Boolean(model_error),
              )}
              selection={session.model_selection}
              providers={application?.providers ?? []}
              model_switch_disabled_reason={is_idle_for_model ? undefined : '存在活动运行或排队输入时不能切换模型'}
              on_effort_change={(effort) => store.setSessionReasoningEffort(session.session_id, effort)}
              on_model_change={(selection) => store.setSessionModel(session.session_id, selection)}
              on_open_change={(open) => {
                setShowHelp(false);
                if (open) setInitialSettingsCategory(null);
                updateActiveOverlay('model', open);
              }}
              open={active_overlay === 'model'}
              trigger_class_name={styles.model_selector}
            />
            <button
              aria-label={edit ? '发送修改' : normal_send_label}
              title={edit ? '发送修改' : show_send_shortcut ? sendShortcutLabel(Boolean(feedback)) : undefined}
              className={styles.send_button}
              data-action={primary_action}
              disabled={
                edit ? !edit_can_send : (has_composer_content && (Boolean(feedback) || Boolean(intervention))) ||
                (store.composer_pending && !compaction_cancels_from_primary) ||
                attachment_flow.pending ||
                !primary_action_available ||
                store.pending_compaction_cancel_session_id === session.session_id ||
                (has_composer_content && model_required && !is_control_command && !compaction_cancels_from_primary)
              }
              onClick={() => {
                if (edit) {
                  void submitDraft();
                } else if (compaction_cancels_from_primary && manual_compaction) {
                  void store.cancelSessionCompaction(session.session_id, manual_compaction.compaction_id);
                } else if (has_composer_content) {
                  void submitDraft();
                } else if (goal_stops_from_primary && goal) {
                  void store.stopGoal(session.session_id, goal.goal_id, goal.generation);
                } else if (active_run_id) {
                  void store.interruptRun(session.session_id, active_run_id);
                }
              }}
              type="button"
            >
              <Icon
                name={
                  compaction_cancels_from_primary || goal_stops_from_primary || run_interrupts_from_primary
                    ? 'stop'
                    : 'arrow-down'
                }
                size={compaction_cancels_from_primary || goal_stops_from_primary || run_interrupts_from_primary ? 28 : 20}
              />
            </button>
          </footer>
        </section>
      )}
    </div>
  );
});

function historySkillTag(edit: HistoryEditDraft): { name: string } | null {
  if (edit.skill_edit.type === 'select') return { name: edit.skill_edit.name };
  if (edit.skill_edit.type === 'clear') return null;
  return edit.basis.message.skill ?? null;
}

const HistoryEditNotice = observer(function HistoryEditNotice({ editor, connected }: Readonly<{
  editor: HistoryEditStore;
  connected: boolean;
}>) {
  const edit = editor.draft;
  if (!edit) return null;
  if (edit.phase === 'submitting') return <ComposerNotice tone="neutral">正在发送修改…</ComposerNotice>;
  if (edit.phase === 'uncertain') return (
    <ComposerNotice tone="warning" action={connected ? {
      label: '重试原发送', onClick: () => void editor.retry(),
    } : undefined}>发送结果尚未确认。</ComposerNotice>
  );
  if (editor.sessionBusy) return <ComposerNotice tone="warning">会话当前无法编辑历史消息。</ComposerNotice>;
  if (editor.stale) return (
    <ComposerNotice tone="warning" action={connected && !editor.busyTarget ? {
      label: '重新核对', onClick: () => void editor.recheck(),
    } : undefined}>{edit.error ?? '历史已更新，请重新核对后发送。'}</ComposerNotice>
  );
  return edit.error ? <ComposerNotice tone="error">{edit.error}</ComposerNotice> : null;
});

function EditContextMenu(props: Readonly<{
  disabled: boolean;
  on_attachment: () => void;
  on_skill: () => void;
  on_mcp: () => void;
}>) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger aria-label="添加输入内容" className={styles.attachment_button} disabled={props.disabled} iconOnly variant="text">
        <Icon name="plus" size={17} />
      </DropdownMenuTrigger>
      <DropdownMenuContent aria-label="添加输入内容" align="start" className={styles.hosting_menu}>
        <DropdownMenuItem onSelect={props.on_attachment}>添加附件</DropdownMenuItem>
        <DropdownMenuItem onSelect={props.on_skill}>选择 Skill</DropdownMenuItem>
        <DropdownMenuItem onSelect={props.on_mcp}>选择 MCP</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const NewSessionDraftComposer = observer(function NewSessionDraftComposer({
  draft_key,
}: Readonly<{
  draft_key: NewSessionDraftKey;
}>) {
  const store = useRootStore();
  const application = store.projection.application;
  const drafts = store.new_session_drafts;
  const draft = drafts.get(draft_key);
  const [skill_picker_open, setSkillPickerOpen] = useState(false);
  const [mcp_picker_open, setMcpPickerOpen] = useState(false);
  const [workspace_picker_open, setWorkspacePickerOpen] = useState(false);
  const [agent_picker_open, setAgentPickerOpen] = useState(false);
  const [agent_options, setAgentOptions] = useState<readonly CustomAgentSnapshot[]>([]);
  const [agent_loading, setAgentLoading] = useState(false);
  const [agent_center_unavailable, setAgentCenterUnavailable] = useState(false);
  const [agent_preparation, setAgentPreparation] = useState<PreparedCustomAgentSnapshot | null>(null);
  const [agent_preparing, setAgentPreparing] = useState(false);
  const [agent_install, setAgentInstall] = useState<PreparedCustomAgentSnapshot | null>(null);
  const agent_request = useRef(0);
  const [quick_action_loading, setQuickActionLoading] = useState(false);
  const [quick_action_confirmation, setQuickActionConfirmation] = useState<{
    action: QuickActionConfig;
    resolved: ResolvedQuickAction;
    owner: NewSessionDraft;
    changes: string[];
  } | null>(null);
  const quick_action_request = useRef(0);
  const [conflict_target, setConflictTarget] = useState<string | null>(null);
  const [active_overlay, setActiveOverlay] = useState<'execution' | 'model' | null>(null);
  const [show_help, setShowHelp] = useState(false);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const textarea_ref = useRef<HTMLTextAreaElement>(null);
  const slash_ref = useRef<HTMLDivElement>(null);
  const input_method = useInputMethodGuard();
  const attachment_flow = useComposerAttachments({
    attachments: draft?.attachments ?? [],
    disabled: store.composer_pending || draft?.materialization_attempt !== null,
    on_attachments_change: (attachments) => store.new_session_drafts.updateAttachments(draft_key, attachments),
    on_error: (message) => store.showInteractionError(message),
    owner_key: draft_key,
  });
  const selected_skill = draft?.selected_skill_name ? { name: draft.selected_skill_name } : null;
  const selected_skill_unavailable = Boolean(selected_skill && draft?.skill_status === 'ready'
    && !draft.skill_options.some((skill) => skill.name === selected_skill.name));
  const selected_skill_pending = Boolean(selected_skill && draft?.skill_status !== 'ready');
  const selected_mcp_unavailable = draft?.selected_mcp_availability === 'unavailable';
  const capability_blocked = selected_skill_unavailable || selected_skill_pending || selected_mcp_unavailable;
  const slash_query = draft?.text.startsWith('/') && !draft.text.includes('\n') ? draft.text.toLocaleLowerCase() : null;
  const slash_items: readonly SlashCommandItem[] =
    slash_query === null
      ? []
      : SLASH_COMMANDS.filter(
          (item) =>
            item.name.includes(slash_query) || item.description.toLocaleLowerCase().includes(slash_query.slice(1)),
        ).map((item) => ({ ...item, disabled_reason: draftSlashDisabledReason(item.name) }));

  useEffect(() => {
    const first_enabled = slash_items.findIndex((item) => !item.disabled_reason);
    const exact_index = slash_items.findIndex((item) => item.name === slash_query);
    setSlashActiveIndex(exact_index >= 0 ? exact_index : Math.max(0, first_enabled));
  }, [slash_query]);
  useEffect(() => {
    ++quick_action_request.current;
    setQuickActionLoading(false);
    setQuickActionConfirmation(null);
    setSkillPickerOpen(false);
    setWorkspacePickerOpen(false);
    setAgentPickerOpen(false);
    setActiveOverlay(null);
    setShowHelp(false);
  }, [draft_key]);
  useEffect(() => {
    setMcpPickerOpen(false);
  }, [draft_key, draft?.variant]);
  useEffect(
    () => () => {
      // 未提交草稿离开 owner 时清理 MCP；结果未知的物化请求必须保留原幂等键和冻结载荷。
      const leaving = drafts.get(draft_key);
      if (leaving?.selected_mcp && !leaving.materialization_attempt)
        drafts.updateSelectedMcp(draft_key, null);
    },
    [draft_key, drafts],
  );
  useEffect(() => resizeTextarea(textarea_ref.current), [draft?.text]);
  useEffect(() => {
    if (!agent_picker_open || !store.runtime_client) return;
    const client = store.runtime_client;
    let active = true;
    setAgentOptions([]);
    setAgentLoading(true);
    setAgentCenterUnavailable(false);
    void client.command({ type: 'list_custom_agents', payload: {} }).then((result) => {
      if (active && client === store.runtime_client) {
        setAgentOptions(result.payload.agents);
        setAgentCenterUnavailable(result.payload.center_unavailable);
      }
    }).catch((cause: unknown) => {
      if (active) store.showInteractionError(cause instanceof Error ? cause.message : '无法读取智能体目录。');
    }).finally(() => {
      if (active && client === store.runtime_client) setAgentLoading(false);
    });
    return () => { active = false; };
  }, [agent_picker_open, store.runtime_client]);
  useEffect(() => {
    const selection = draft?.selected_agent;
    const client = store.runtime_client;
    const request = ++agent_request.current;
    setAgentPreparation(null);
    setAgentPreparing(false);
    if (!selection || !client) return;
    setAgentPreparing(true);
    void client.command({ type: 'prepare_custom_agent', payload: {
      selection, ...(draft?.workspace_id ? { workspace_id: draft.workspace_id } : {}),
    } }).then((result) => {
      const current = store.new_session_drafts.get(draft_key);
      if (request === agent_request.current && client === store.runtime_client && current
        && sameAgent(current.selected_agent, selection) && current.workspace_id === draft?.workspace_id) {
        setAgentPreparation(result.payload);
      }
    }).catch((cause: unknown) => {
      if (request === agent_request.current && client === store.runtime_client)
        store.showInteractionError(cause instanceof Error ? cause.message : '无法检查智能体依赖。');
    }).finally(() => {
      if (request === agent_request.current) setAgentPreparing(false);
    });
  }, [draft_key, draft?.selected_agent?.name, draft?.selected_agent?.source.type,
    draft?.selected_agent?.source.type === 'center' ? draft.selected_agent.source.center_id : null,
    draft?.workspace_id, store.runtime_client]);

  const draft_model = draft?.model_selection ?? application?.model_settings.default_model ?? null;
  const management = application?.model_settings.management;
  const model_issue = management && draft_model
    ? modelSelectionIssue(draft_model, application.providers, application.model_settings)
    : management?.unavailable_reason;
  const effort_options = useDraftModelEfforts(draft_model, active_overlay === 'model');

  if (!draft) return null;
  const current_draft = draft;
  const model_required = draft_model === null || Boolean(model_issue);
  const can_send = Boolean(draft.text.trim() || draft.attachments.length || draft.quotes.length);
  const unresolved = draft.materialization_attempt !== null;
  const active_workspaces = application?.workspaces.filter((workspace) => workspace.lifecycle === 'active') ?? [];
  const quick_actions_disabled = store.composer_pending || attachment_flow.pending || unresolved || quick_action_loading;

  function applyQuickAction(action: QuickActionConfig, resolved: ResolvedQuickAction, owner: NewSessionDraft) {
    const latest = store.new_session_drafts.get(draft_key);
    if (!latest || !sameQuickActionDraft(owner, latest) || store.navigation.selected_draft_key !== draft_key) {
      setQuickActionConfirmation(null);
      store.showInteractionError('草稿已变化，请重新选择快捷入口。');
      return;
    }
    if (resolved.agent) store.new_session_drafts.updateSelectedAgent(draft_key, resolved.agent);
    if (resolved.skill) {
      store.new_session_drafts.updateSelectedSkill(draft_key, resolved.skill.name);
      store.new_session_drafts.applySkillOptions(draft_key, [
        ...latest.skill_options.filter((option) => option.name !== resolved.skill?.name), resolved.skill,
      ]);
    }
    if (resolved.mcp) store.new_session_drafts.updateSelectedMcp(draft_key, resolved.mcp);
    if (action.template) store.new_session_drafts.updateText(draft_key,
      latest.text.trim() ? `${latest.text.trimEnd()}\n\n${action.template}` : action.template);
    setQuickActionConfirmation(null);
    setSkillPickerOpen(action.picker === 'skill');
    setMcpPickerOpen(action.picker === 'mcp');
    setAgentPickerOpen(action.picker === 'agent');
    if (!action.picker) requestAnimationFrame(() => textarea_ref.current?.focus());
  }

  async function selectQuickAction(action: QuickActionConfig) {
    const owner = store.new_session_drafts.get(draft_key);
    const client = store.runtime_client;
    if (!owner || !client || quick_actions_disabled) return;
    const request = ++quick_action_request.current;
    setQuickActionLoading(true);
    try {
      const resolved = await resolveQuickAction(action, {
        agents: async () => (await client.command({ type: 'list_custom_agents', payload: {} })).payload.agents,
        skills: async () => (await store.listSkills(owner.workspace_id)).skills,
        mcp: () => store.listMcpServerOptions({
          context: { type: 'new_session', payload: { workspace_id: owner.workspace_id ?? undefined } },
          variant: owner.variant,
        }),
      });
      if (request !== quick_action_request.current || client !== store.runtime_client
        || store.navigation.selected_draft_key !== draft_key) return;
      const latest = store.new_session_drafts.get(draft_key);
      if (!latest || !sameQuickActionDraft(owner, latest)) {
        store.showInteractionError('草稿已变化，请重新选择快捷入口。');
        return;
      }
      const changes = [
        action.template && latest.text.trim() ? '在现有文字后追加模板' : null,
        resolved.agent && latest.selected_agent && !sameAgent(latest.selected_agent, resolved.agent)
          ? `将智能体切换为「${resolved.agent.name}」` : null,
        resolved.skill && latest.selected_skill_name && latest.selected_skill_name !== resolved.skill.name
          ? `将技能切换为「${resolved.skill.name}」` : null,
        resolved.mcp && latest.selected_mcp && latest.selected_mcp.server_key !== resolved.mcp.server_key
          ? `将 MCP 服务切换为「${resolved.mcp.display_name}」` : null,
      ].filter((item): item is string => Boolean(item));
      if (changes.length) setQuickActionConfirmation({ action, resolved, owner: latest, changes });
      else applyQuickAction(action, resolved, latest);
    } catch (cause: unknown) {
      if (request === quick_action_request.current && client === store.runtime_client)
        store.showInteractionError(cause instanceof Error ? cause.message : '无法应用快捷入口。');
    } finally {
      if (request === quick_action_request.current) setQuickActionLoading(false);
    }
  }

  function updateOverlay(overlay: 'execution' | 'model', open: boolean) {
    setActiveOverlay((current) => (open ? overlay : current === overlay ? null : current));
  }

  function handleSlashCommand(command: SlashCommandItem) {
    if (command.disabled_reason) {
      store.new_session_drafts.updateText(draft_key, '');
      store.showInteractionError(command.disabled_reason);
      return;
    }
    store.new_session_drafts.updateText(draft_key, '');
    if (command.name === '/goal') {
      store.new_session_drafts.updateGoalArmed(draft_key, true);
    } else if (command.name === '/skill') {
      setSkillPickerOpen(true);
      return;
    } else if (command.name === '/mcp') {
      setSkillPickerOpen(false);
      setMcpPickerOpen(true);
      return;
    } else if (command.name === '/model') {
      setActiveOverlay('model');
    } else if (command.name === '/mode' || command.name === '/approval') {
      setActiveOverlay('execution');
    } else if (command.name === '/new') {
      store.openNewSessionDraft(current_draft.workspace_id, store.navigation.target_device);
    } else {
      setShowHelp(true);
    }
    requestAnimationFrame(() => textarea_ref.current?.focus());
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) return;
    if (slash_items.length > 0 && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
      event.preventDefault();
      const direction = event.key === 'ArrowDown' ? 1 : -1;
      const next = nextEnabledSlashIndex(slash_items, slash_active_index, direction);
      setSlashActiveIndex(next);
      slash_ref.current
        ?.querySelector<HTMLElement>(`[data-slash-index="${next}"]`)
        ?.scrollIntoView({ block: 'nearest' });
      return;
    }
    if (event.key === 'Escape' && slash_query !== null) {
      event.preventDefault();
      store.new_session_drafts.updateText(draft_key, current_draft.text.slice(1));
      return;
    }
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      if (event.ctrlKey || event.metaKey) {
        if (isInterventionShortcut(event) && application?.capabilities.input_intervention
          && !store.projection.is_stale && slash_query === null && !current_draft.goal_armed
          && parseSessionCommand(current_draft.text).type === 'not_command'
          && can_send && !unresolved && !store.composer_pending && !attachment_flow.pending && !agent_preparing) {
          // 首次输入已经走物化命令，新会话没有需要打断的 Run。
          void submitNewDraft();
        }
        return;
      }
      const selected_command = slash_query === null ? undefined : slash_items[slash_active_index];
      if (selected_command) {
        handleSlashCommand(selected_command);
        return;
      }
      const exact_command = SLASH_COMMANDS.find(
        (item) => item.name === event.currentTarget.value.trim().toLocaleLowerCase(),
      );
      const command = exact_command
        ? { ...exact_command, disabled_reason: draftSlashDisabledReason(exact_command.name) }
        : undefined;
      if (command) handleSlashCommand(command);
      else submitNewDraft();
    }
  }

  async function submitNewDraft() {
    if (unresolved) {
      void store.materializeNewSessionDraft(draft_key);
      return;
    }
    if (capability_blocked) return;
    const command = slash_items.find((item) => item.name === current_draft.text.trim().toLocaleLowerCase());
    if (command) {
      handleSlashCommand(command);
      return;
    }
    if (parseSessionCommand(current_draft.text).type !== 'not_command') {
      store.showInteractionError('发送第一条消息后，可在会话中使用刷新指令');
      return;
    }
    if (model_required) return;
    const selection = current_draft.selected_agent;
    const client = store.runtime_client;
    if (selection && client) {
      const request = ++agent_request.current;
      setAgentPreparing(true);
      try {
        const result = await client.command({ type: 'prepare_custom_agent', payload: {
          selection, ...(current_draft.workspace_id ? { workspace_id: current_draft.workspace_id } : {}),
        } });
        const latest = store.new_session_drafts.get(draft_key);
        if (request !== agent_request.current || client !== store.runtime_client || !latest
          || store.navigation.selected_draft_key !== draft_key || !sameAgent(latest.selected_agent, selection)
          || latest.workspace_id !== current_draft.workspace_id || latest !== current_draft) return;
        setAgentPreparation(result.payload);
        if (result.payload.dependencies.some((item) => agentDependencyBlocksSession(item.status))) {
          if (result.payload.install_candidates.length) setAgentInstall(result.payload);
          else store.showInteractionError('智能体依赖尚未就绪。请查看缺项，完成配置或选择普通会话后重试。');
          return;
        }
      } catch (cause: unknown) {
        if (request === agent_request.current && client === store.runtime_client)
          store.showInteractionError(cause instanceof Error ? cause.message : '无法检查智能体依赖。');
        return;
      } finally { if (request === agent_request.current) setAgentPreparing(false); }
    }
    void store.materializeNewSessionDraft(draft_key);
  }

  async function retryAgentInstall() {
    setAgentInstall(null);
    const latest = store.new_session_drafts.get(draft_key);
    const client = store.runtime_client;
    if (!latest?.selected_agent || !client) return;
    const request = ++agent_request.current;
    setAgentPreparing(true);
    try {
      const result = await client.command({ type: 'prepare_custom_agent', payload: {
        selection: latest.selected_agent, ...(latest.workspace_id ? { workspace_id: latest.workspace_id } : {}),
      } });
      if (request !== agent_request.current || client !== store.runtime_client
        || store.new_session_drafts.get(draft_key) !== latest) return;
      setAgentPreparation(result.payload);
      if (result.payload.dependencies.every((item) => item.status === 'available')) return;
      if (result.payload.install_candidates.length) setAgentInstall(result.payload);
      else if (result.payload.dependencies.some((item) => agentDependencyBlocksSession(item.status)))
        store.showInteractionError('依赖仍未就绪，请查看缺项或打开插件管理。');
    } catch (cause: unknown) {
      if (request === agent_request.current && client === store.runtime_client)
        store.showInteractionError(cause instanceof Error ? cause.message : '无法重新检查智能体依赖。');
    } finally { if (request === agent_request.current) setAgentPreparing(false); }
  }

  return (
    <div className={`${styles.dock} ${styles.new_session_dock}`}>
      <div className={styles.composer_top}>
        {store.interaction_error && (
          <ComposerNotice tone="error" dismiss={{ label: '关闭错误提示', onClick: () => store.clearInteractionError() }}>
            {store.interaction_error}
          </ComposerNotice>
        )}
        {unresolved && <ComposerNotice tone="warning">首次发送结果待确认。草稿和附件已冻结；重试会先查询原请求。</ComposerNotice>}
        <Dialog
          aria_label="草稿目标冲突"
          backdrop_class_name={styles.draft_conflict_backdrop}
          dialog_class_name={styles.draft_conflict_dialog}
          on_close={() => setConflictTarget(null)}
          open={conflict_target !== null}
        >
          <h3>该项目已有草稿</h3>
          <p>当前内容不会覆盖已有草稿。你可以取消切换，或打开目标草稿；当前草稿会保留。</p>
          <footer>
            <Button onClick={() => setConflictTarget(null)} variant="outlined">取消</Button>
            <Button onClick={() => {
              const target = conflict_target;
              setConflictTarget(null);
              store.openNewSessionDraft(target || null);
            }} variant="primary">打开目标草稿</Button>
          </footer>
        </Dialog>
        {store.runtime_client && agent_install && <PluginOperationDialog key={`${agent_install.revision}:${agent_request.current}`} client={store.runtime_client}
          intent={{ type: 'install', packages: agent_install.install_candidates }} on_close={() => { setAgentInstall(null); }}
          on_complete={() => { store.showInteractionError('插件已安装。请关闭进度窗后再次发送，系统会重新核对依赖。'); }}
          on_retry={() => void retryAgentInstall()} />}
        <Dialog
          aria_label="应用快捷入口"
          backdrop_class_name={styles.draft_conflict_backdrop}
          dialog_class_name={styles.draft_conflict_dialog}
          on_close={() => setQuickActionConfirmation(null)}
          open={quick_action_confirmation !== null}
        >
          <h3>应用「{quick_action_confirmation?.action.label}」？</h3>
          <p>{quick_action_confirmation?.changes.join('；')}。当前附件和已有文字会保留。</p>
          <footer>
            <Button onClick={() => setQuickActionConfirmation(null)} variant="outlined">取消</Button>
            <Button onClick={() => {
              const pending = quick_action_confirmation;
              if (pending) applyQuickAction(pending.action, pending.resolved, pending.owner);
            }} variant="primary">确认应用</Button>
          </footer>
        </Dialog>
        <div className={styles.draft_intro}>
          <h1>今天想完成什么？</h1>
          <p>选择项目，或直接描述你想完成的任务。</p>
          <div className={styles.draft_quick_actions} aria-label="新会话快捷入口">
            {QUICK_ACTIONS.map((action) => <button disabled={quick_actions_disabled} key={action.id}
              onClick={() => void selectQuickAction(action)} type="button">
              <Icon name={action.icon} size={17} />{action.label}
            </button>)}
          </div>
        </div>
        {draft.selected_agent && agent_preparing && <ComposerNotice tone="neutral">正在检查智能体依赖…</ComposerNotice>}
        {draft.selected_agent && agent_preparation && agent_preparation.dependencies.some((item) => agentDependencyBlocksSession(item.status)) &&
          <ComposerNotice tone="warning">
            <div className={styles.agent_dependencies}>
              <span>智能体依赖待处理：{agent_preparation.dependencies.filter((item) => agentDependencyBlocksSession(item.status)).map((item) => `${item.kind} ${item.reference.name}（${agentDependencyLabel(item.status)}）`).join('、')}</span>
              <div className={styles.agent_dependency_actions}>
                {!store.remote_device && agent_preparation.install_candidates.length > 0 && <Button onClick={() => setAgentInstall(agent_preparation)} size="small" variant="text">下载缺少的插件</Button>}
                {!store.remote_device && <Button onClick={() => store.settings.open('plugins')} size="small" variant="text">管理插件</Button>}
                <Button disabled={agent_preparing} onClick={() => void retryAgentInstall()} size="small" variant="text">重新检查依赖</Button>
              </div>
            </div>
          </ComposerNotice>}
        {draft.selected_agent && !agent_preparing && !agent_preparation &&
          <ComposerNotice tone="warning" action={{ label: '重新检查依赖', onClick: () => void retryAgentInstall() }}>暂未取得智能体依赖状态。</ComposerNotice>}
        <section aria-label="新会话项目选项" className={`${styles.secondary_drawer} ${styles.draft_context_drawer}`}>
          <DevicePicker disabled={quick_actions_disabled} />
          <WorkspacePicker
              disabled={quick_actions_disabled}
              has_draft={(workspace_id) => store.new_session_drafts.hasDraft(draftKeyForWorkspace(workspace_id))
                && draft_key !== draftKeyForWorkspace(workspace_id)}
              on_create={() => store.addWorkspace()}
              on_open_change={(open) => { setWorkspacePickerOpen(open); if (open) setAgentPickerOpen(false); }}
              on_select={(workspace_id) => {
                if (!store.moveNewSessionDraft(draft_key, workspace_id, attachment_flow.pending))
                  setConflictTarget(workspace_id ?? '');
              }}
              open={workspace_picker_open}
              selected_workspace_id={draft.workspace_id}
              workspaces={active_workspaces}
          />
          <ContextPicker
            action_icon="settings" action_label="管理智能体" dialog_label="选择智能体"
            disabled={quick_actions_disabled} icon="bot" label="智能体"
            empty_text={agent_loading ? '正在读取智能体…' : undefined}
            notice={agent_center_unavailable ? '企业目录暂不可读取，当前仅显示最后一次有效结果。' : undefined}
            on_action={store.remote_device ? undefined : () => store.settings.open('agents')}
            on_open_change={(open) => { setAgentPickerOpen(open); if (open) setWorkspacePickerOpen(false); }}
            on_select={(value) => {
              const agent = agent_options.find((item) => agentOptionValue({ name: item.config.name, source: item.source }) === value);
              if (value === null || agent) store.new_session_drafts.updateSelectedAgent(draft_key,
                agent ? { name: agent.config.name, source: agent.source } : null);
            }}
            open={agent_picker_open}
            options={agent_options.map((agent) => ({
              value: agentOptionValue({ name: agent.config.name, source: agent.source }),
              label: agent.config.name,
              hints: [agent.source.type === 'center' ? '企业' : '个人'],
              search_text: `${agent.config.name} ${agent.config.description}`,
            }))}
            selected={draft.selected_agent ? { value: agentOptionValue(draft.selected_agent),
              label: draft.selected_agent.name, search_text: draft.selected_agent.name } : null}
            trigger_label="选择智能体"
          />
        </section>
      </div>
      <section className={styles.composer}>
        <SlashCommandMenu
          active_index={slash_active_index}
          items={slash_items}
          menu_ref={slash_ref}
          on_select={handleSlashCommand}
          open={slash_items.length > 0}
        />
        <SlashCommandHelp on_close={() => setShowHelp(false)} open={show_help} />
        {skill_picker_open && (
          <SkillPicker
            key={draft_key}
            workspace_id={draft.workspace_id}
            on_close={() => {
              setSkillPickerOpen(false);
              requestAnimationFrame(() => textarea_ref.current?.focus());
            }}
            on_select={(skill) => {
              store.new_session_drafts.updateSelectedSkill(draft_key, skill.name);
              store.new_session_drafts.applySkillOptions(draft_key, [
                ...draft.skill_options.filter((option) => option.name !== skill.name), skill,
              ]);
              setSkillPickerOpen(false);
              requestAnimationFrame(() => textarea_ref.current?.focus());
            }}
          />
        )}
        {mcp_picker_open && (
          <McpServerPicker
            key={`${draft_key}:${draft.variant}`}
            on_close={() => {
              setMcpPickerOpen(false);
              requestAnimationFrame(() => textarea_ref.current?.focus());
            }}
            on_select={(server) => {
              store.new_session_drafts.updateSelectedMcp(draft_key, server);
              setMcpPickerOpen(false);
              requestAnimationFrame(() => textarea_ref.current?.focus());
            }}
            request={{
              context: { type: 'new_session', payload: { workspace_id: draft.workspace_id ?? undefined } },
              variant: draft.variant,
            }}
          />
        )}
        {model_issue && <ComposerNotice tone="error">{model_issue}</ComposerNotice>}
        <ComposerAttachmentContext
          attachments={attachment_flow.attachments}
          on_remove={attachment_flow.remove}
          owner_key={draft_key}
          paste_pending={attachment_flow.paste_pending}
          session_id={null}
        />
        {(draft.goal_armed || selected_skill || draft.selected_mcp) && (
          <div className={styles.draft_tags}>
            {draft.goal_armed && (
              <span>
                目标
                <button
                  aria-label="取消目标标记"
                  disabled={unresolved}
                  onClick={() => store.new_session_drafts.updateGoalArmed(draft_key, false)}
                  type="button"
                >
                  <Icon name="x" size={12} />
                </button>
              </span>
            )}
            {selected_skill && (
              <span title={selected_skill.name}>
                {selected_skill.name}{selected_skill_unavailable ? ' · 当前项目不可用' : selected_skill_pending ? ' · 验证中' : ''}
                <button
                  aria-label={`移除技能 ${selected_skill.name}`}
                  disabled={unresolved}
                  onClick={() => store.new_session_drafts.updateSelectedSkill(draft_key, null)}
                  type="button"
                >
                  <Icon name="x" size={12} />
                </button>
              </span>
            )}
            {draft.selected_mcp && (
              <span title={draft.selected_mcp.server_key}>
                MCP · {draft.selected_mcp.display_name}
                {draft.selected_mcp_availability === 'unverified' ? ' · 待验证' : ''}
                {draft.selected_mcp_availability === 'unavailable' ? ' · 当前项目不可用' : ''}
                <button
                  aria-label={`移除 MCP ${draft.selected_mcp.display_name}`}
                  disabled={unresolved}
                  onClick={() => store.new_session_drafts.updateSelectedMcp(draft_key, null)}
                  type="button"
                >
                  <Icon name="x" size={12} />
                </button>
              </span>
            )}
          </div>
        )}
        <AppComposerDraftReceiver text={draft.text} has_other_content={draft.attachments.length > 0 || draft.quotes.length > 0} on_fill={(text) => drafts.updateText(draft_key,text)} />
          <textarea
          aria-label="输入消息"
          disabled={
            model_required ||
            store.connection.state !== 'connected' ||
            store.composer_pending ||
            unresolved ||
            attachment_flow.pending
          }
          onChange={(event) => store.new_session_drafts.updateText(draft_key, event.target.value)}
          onCompositionEnd={input_method.onCompositionEnd}
          onCompositionStart={input_method.onCompositionStart}
          onKeyDown={handleKeyDown}
          onKeyUp={input_method.onKeyUp}
          onPaste={(event) =>
            handleComposerPaste(
              event,
              draft.text,
              (value) => store.new_session_drafts.updateText(draft_key, value),
              attachment_flow.pasteImages,
            )
          }
          placeholder={model_required ? '请先选择一个可用模型' : '输入消息…  / 使用指令'}
          ref={textarea_ref}
          rows={2}
          value={draft.text}
        />
        <footer className={styles.composer_actions}>
          <Tooltip content="添加附件">
            <Button
              aria-label="添加附件"
              className={styles.attachment_button}
              disabled={store.composer_pending || attachment_flow.pending || unresolved}
              iconOnly
              onClick={() => void attachment_flow.choose()}
              variant="text"
            >
              <Icon name="plus" size={17} />
            </Button>
          </Tooltip>
          <ExecutionSettingsPopover
            approval_mode={draft.approval_mode}
            disabled={store.composer_pending || unresolved}
            initial_category={null}
            on_approval_change={(mode) => {
              store.setNewSessionDraftApprovalMode(draft_key, mode);
              return Promise.resolve(true);
            }}
            on_open_change={(open) => updateOverlay('execution', open)}
            on_variant_change={(variant) => {
              store.new_session_drafts.updateVariant(draft_key, variant);
              return Promise.resolve(true);
            }}
            open={active_overlay === 'execution'}
            trigger_class_name={styles.execution_selector}
            variant={draft.variant}
          />
          <span className={styles.action_spacer} />
          <ModelSettingsPopover
            disabled={store.composer_pending || unresolved}
            effort={draft.reasoning_effort}
            effort_options={effort_options}
            initial_category={null}
            model_display_name={composerModelLabel(application, draft.model_selection, draft_model, Boolean(model_issue))}
            selection={draft.model_selection}
            providers={application?.providers ?? []}
            on_effort_change={(effort) => {
              store.new_session_drafts.updateReasoningEffort(draft_key, effort);
              return Promise.resolve(true);
            }}
            on_model_change={(selection) => {
              store.setNewSessionDraftModel(draft_key, selection);
              return Promise.resolve(true);
            }}
            on_open_change={(open) => updateOverlay('model', open)}
            open={active_overlay === 'model'}
            trigger_class_name={styles.model_selector}
          />
          <button
            aria-label={unresolved ? "重试确认首次发送" : "发送消息"}
            className={styles.send_button}
            data-action="send"
            title={application?.capabilities.input_intervention && !unresolved && !draft.goal_armed
              && slash_query === null ? sendShortcutLabel() : undefined}
            disabled={!can_send || (!unresolved && (model_required || capability_blocked)) || store.composer_pending || attachment_flow.pending || agent_preparing}
            onClick={() => void submitNewDraft()}
            type="button"
          >
            <Icon name="arrow-down" size={20} />
          </button>
        </footer>
      </section>
    </div>
  );
});

function isMacPlatform(): boolean {
  return navigator.platform.toLocaleLowerCase().includes('mac');
}

function isInterventionShortcut(event: React.KeyboardEvent<HTMLTextAreaElement>): boolean {
  return !event.altKey && !event.repeat && (event.ctrlKey || (event.metaKey && isMacPlatform()));
}

function interventionShortcutLabel(): string {
  return isMacPlatform() ? 'Command + Enter' : 'Ctrl + Enter';
}

function sendShortcutLabel(feedback = false): string {
  const intervention = `介入（${interventionShortcutLabel()}）`;
  return feedback ? intervention : `发送（Enter）· ${intervention}`;
}

function agentOptionValue(selection: CustomAgentSelection): string {
  return JSON.stringify([selection.source.type,
    selection.source.type === 'center' ? selection.source.center_id : null, selection.name]);
}

function sameAgent(left: CustomAgentSelection | null, right: CustomAgentSelection | null): boolean {
  if (!left || !right) return left === right;
  return left.name === right.name && left.source.type === right.source.type
    && (left.source.type !== 'center' || right.source.type === 'center' && left.source.center_id === right.source.center_id);
}

/** 目录/依赖刷新会替换草稿快照；只用会被快捷入口覆盖的用户意图判断异步结果是否过期。 */
function sameQuickActionDraft(before: NewSessionDraft, current: NewSessionDraft): boolean {
  return before.key === current.key && before.workspace_id === current.workspace_id
    && before.variant === current.variant && before.text === current.text
    && before.selected_skill_name === current.selected_skill_name
    && before.selected_mcp?.server_key === current.selected_mcp?.server_key
    && sameAgent(before.selected_agent, current.selected_agent)
    && before.materialization_attempt === current.materialization_attempt;
}

// 与 Runtime 首次物化一致：环境故障只提示，安装、启用和来源冲突仍需处理。
function agentDependencyBlocksSession(status: PreparedCustomAgentSnapshot['dependencies'][number]['status']): boolean {
  return status === 'plugin_missing' || status === 'plugin_disabled'
    || status === 'capability_disabled' || status === 'name_conflict';
}

function agentDependencyLabel(status: PreparedCustomAgentSnapshot['dependencies'][number]['status']): string {
  switch (status) {
    case 'available': return '可用';
    case 'plugin_missing': return '缺少插件';
    case 'plugin_disabled': return '插件未启用';
    case 'capability_disabled': return '能力未启用';
    case 'capability_missing': return '能力缺失';
    case 'name_conflict': return '名称冲突';
    case 'configuration_missing': return '缺少配置';
    case 'software_missing': return '缺少软件';
    case 'runtime_unavailable': return '运行环境不可用';
  }
}

function ContextUsageRing({
  context,
}: Readonly<{
  context: SessionViewSnapshot['usage']['context'];
}>) {
  let degrees = 0;
  let label = '上下文占用未知';
  if (context && context.used_tokens !== null && context.usage_basis_points !== null) {
    const prefix = context.accuracy === 'estimated' ? '约 ' : '';
    degrees = Math.min(360, Math.max(0, context.usage_basis_points * 0.036));
    label = `${prefix}${formatCompact(context.used_tokens)} / ${formatCompact(context.window_tokens)} · ${(context.usage_basis_points / 100).toFixed(1)}%`;
  }
  if (context?.calibration_pending) label += " · 待校准";
  return (
    <Tooltip content={label}>
      <span
        aria-label={`上下文用量：${label}`}
        className={styles.context_ring}
        role="img"
        style={{ '--context-degrees': `${degrees}deg` } as React.CSSProperties}
        tabIndex={0}
      />
    </Tooltip>
  );
}

function compactionStatusLabel(
  trigger: NonNullable<SessionViewSnapshot['session']['active_compaction']>['trigger'],
): string {
  if (trigger.type === 'manual') {
    return '正在压缩上下文';
  }
  return trigger.reason === 'provider_overflow' ? '模型报告上下文不足，正在自动压缩' : '上下文达到阈值，正在自动压缩';
}

function ComposerAttachmentContext(
  props: Readonly<{
    attachments: readonly ComposerAttachment[];
    on_remove: (attachment: ComposerAttachment) => void;
    remove_disabled?: boolean;
    paste_pending: boolean;
    session_id: SessionId | null;
    owner_key: string;
    quotes?: readonly QuotedTextSnapshot[];
    on_quote_remove?: (quote: QuotedTextSnapshot) => void;
    on_quote_locate?: (quote: QuotedTextSnapshot) => Promise<boolean>;
  }>,
) {
  const [detail, setDetail] = useState<ComposerAttachment | null>(null);
  const [quote_detail, setQuoteDetail] = useState<QuotedTextSnapshot | null>(null);
  const [order, setOrder] = useState<readonly string[]>([]);
  const previous_owner_key = useRef(props.owner_key);
  const quotes = props.quotes ?? [];
  const context_items = [
    ...props.attachments.map((attachment) => ({
      key: `attachment:${attachment.selection_id}`,
      type: 'attachment' as const,
      attachment,
    })),
    ...quotes.map((quote) => ({ key: `quote:${quote.quote_id}`, type: 'quote' as const, quote })),
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
            if (item.type === 'quote')
              return [
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
                    disabled={props.remove_disabled}
                    className={styles.attachment_remove}
                    onClick={() => {
                      if (quote_detail?.quote_id === item.quote.quote_id) setQuoteDetail(null);
                      props.on_quote_remove?.(item.quote);
                    }}
                    type="button"
                  >
                    <Icon name="x" size={13} />
                  </button>
                </div>,
              ];
            const attachment = item.attachment;
            return [
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
                  title={attachment.state === 'uploading' ? '取消上传' : '移除附件'}
                  onClick={() => {
                    if (detail?.selection_id === attachment.selection_id) setDetail(null);
                    props.on_remove(attachment);
                  }}
                  type="button"
                >
                  <Icon name="x" size={13} />
                </button>
              </div>,
            ];
          })}
          {props.paste_pending && (
            <div className={styles.attachment_pending} role="status">
              正在添加图片…
            </div>
          )}
        </div>
      )}
      <PresenceBoundary present={detail !== null}>
        {detail && (
          <AttachmentDetailDialog attachment={detail} on_close={() => setDetail(null)} session_id={props.session_id} />
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
    .filter((item) => item.kind === 'file' && item.type.toLocaleLowerCase().startsWith('image/'))
    .map((item) => item.getAsFile())
    .filter((file): file is File => file !== null);
  if (images.length === 0) return;

  event.preventDefault();
  const pasted_text = event.clipboardData.getData('text/plain');
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
  textarea.style.height = 'auto';
  const line_height = 23;
  textarea.style.height = `${Math.min(line_height * 8, Math.max(line_height * 2, textarea.scrollHeight))}px`;
}

function composerModelLabel(
  application: ReturnType<typeof useRootStore>['projection']['application'],
  explicit: import('@ez-assistant/protocol').ModelSelection | null,
  effective: import('@ez-assistant/protocol').ModelSelection | null,
  unavailable: boolean,
): string {
  const enterprise = Boolean(application?.model_settings.management);
  const selection = explicit ?? effective;
  const label = selection ? modelSelectionLabel(selection, application?.providers ?? [], enterprise && Boolean(explicit)) : '';
  const follow = enterprise && !explicit;
  const description = follow ? ['跟随组织默认', label].filter(Boolean).join(' · ') : label || '选择模型';
  return unavailable ? `${description}（不可用）` : description;
}

function attachmentStateLabel(attachment: ComposerAttachment): string {
  switch (attachment.state) {
    case 'selected':
      return formatBytes(attachment.size_bytes);
    case 'uploading':
      return '上传中';
    case 'uploaded':
      return '已上传';
    case 'failed':
      return '重试发送';
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
  if (command_name === '/mcp refresh' || command_name === '/skill refresh') {
    if (!session_commands_available) return '当前 Runtime 不支持会话控制指令';
    return null;
  }
  if (command_name === '/title' && view?.session.role === 'controller') return '主控标题固定';
  if (command_name !== '/goal') return null;
  if (view?.goal) return '当前会话已有目标，请先继续或退出现有目标';
  return null;
}

function draftSlashDisabledReason(command_name: string): string | null {
  if (
    command_name === '/compact' ||
    command_name === '/title' ||
    command_name === '/mcp refresh' ||
    command_name === '/skill refresh'
  )
    return '发送第一条消息后可用';
  return null;
}

function nextEnabledSlashIndex(items: readonly SlashCommandItem[], current: number, direction: 1 | -1): number {
  let next = current;
  for (let checked = 0; checked < items.length; checked += 1) {
    next = (next + direction + items.length) % items.length;
    if (!items[next]?.disabled_reason) return next;
  }
  return current;
}

function primaryActionLabel(action: 'send' | 'stop-goal' | 'interrupt' | 'cancel-compaction'): string {
  if (action === 'cancel-compaction') return '终止压缩';
  if (action === 'stop-goal') return '停止目标';
  if (action === 'interrupt') return '中断当前轮次';
  return '发送消息';
}

function resolvePrimaryAction(
  cancel_compaction: boolean,
  stop_goal: boolean,
  interrupt_run: boolean,
): 'send' | 'stop-goal' | 'interrupt' | 'cancel-compaction' {
  if (cancel_compaction) return 'cancel-compaction';
  if (stop_goal) return 'stop-goal';
  if (interrupt_run) return 'interrupt';
  return 'send';
}
