import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useRef, useState } from "react";
import type {
  ApprovalDecision,
  ModelKey,
  SessionViewSnapshot,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ApprovalWorkspace, isAllowDecision } from "./ApprovalWorkspace";
import {
  APPROVAL_OPTIONS,
  formatCompact,
  type PickerName,
  SLASH_COMMANDS,
  type SlashCommand,
  VARIANT_OPTIONS,
} from "./composerOptions";
import { QueueDrawer } from "./QueueDrawer";
import { SlashCommandHelp, SlashCommandMenu } from "./SlashCommandMenu";
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
  const [open_picker, setOpenPicker] = useState<PickerName>(null);
  const [slash_active_index, setSlashActiveIndex] = useState(0);
  const [queue_open, setQueueOpen] = useState(true);
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

  const model_options = useMemo<readonly SelectionOption<ModelKey>[]>(() =>
    (application?.models ?? []).flatMap((model) => model.model_key && model.is_valid ? [{
      value: model.model_key,
      label: model.display_name,
      description: [model.provider ?? model.protocol, model.model, model.context_window_tokens ? formatCompact(model.context_window_tokens) : null]
        .filter(Boolean).join(" · "),
      icon: <Icon name="message" size={15} />,
    }] : []), [application?.models]);

  const slash_query = draft.startsWith("/") && !draft.includes("\n") ? draft.toLocaleLowerCase() : null;
  const slash_items = slash_query === null ? [] : SLASH_COMMANDS.filter((item) =>
    item.name.includes(slash_query) || item.description.toLocaleLowerCase().includes(slash_query.slice(1)),
  );

  useEffect(() => {
    setSlashActiveIndex(0);
  }, [slash_query]);

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
      const exact = SLASH_COMMANDS.find((item) => item.name === slash_query.trim());
      if (exact) {
        handleSlashCommand(exact);
        return;
      }
    }
    const attachment_ids = await attachment_flow.uploadAll(session!.session_id);
    if (!attachment_ids) {
      return;
    }
    if (await store.submitInput(session!.session_id, value, session!.current_variant, attachment_ids)) {
      setDraft("");
      attachment_flow.clear();
    }
  }

  function handleSlashCommand(command: SlashCommand) {
    if (command.picker) {
      setDraft("");
      setOpenPicker(command.picker);
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
      const next = (slash_active_index + direction + slash_items.length) % slash_items.length;
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
        : SLASH_COMMANDS.find((item) => item.name === slash_query.trim());
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

  return (
    <div className={styles.dock}>
      {store.interaction_error && (
        <div className={styles.error_notice} role="alert">
          <span>{store.interaction_error}</span>
          <button aria-label="关闭错误提示" onClick={() => store.clearInteractionError()} type="button"><Icon name="x" size={14} /></button>
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
      ) : (
        <>
          {!read_only && session_view.queue.items.length > 0 && (
            <QueueDrawer open={queue_open} on_open_change={setQueueOpen} queue={session_view.queue} session_id={session.session_id} />
          )}
          {approval && approval_minimized && (
            <button className={styles.approval_restore} onClick={() => setApprovalMinimized(false)} type="button">
              <Icon name="shield" size={16} />
              <span>等待审批</span>
              <b>{session_view.approvals.items.length}</b>
              <Icon name="chevron-down" size={14} />
            </button>
          )}
          {read_only ? null : is_archived ? (
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
                placeholder="输入消息…  / 使用指令"
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
                    <Icon name="paperclip" size={16} />
                  </button>
                </Tooltip>
                <SelectionPopover
                  aria_label="切换执行模式"
                  content_width="content"
                  disabled={store.composer_pending}
                  on_open_change={(open) => setOpenPicker(open ? "variant" : null)}
                  on_select={(variant) => void store.setSessionVariant(session.session_id, variant)}
                  open={open_picker === "variant"}
                  options={VARIANT_OPTIONS}
                  selected={session.current_variant}
                  trigger_class_name={styles.compact_selector}
                />
                <SelectionPopover
                  aria_label="切换审批模式"
                  content_width="content"
                  disabled={store.composer_pending}
                  on_open_change={(open) => setOpenPicker(open ? "approval" : null)}
                  on_select={(mode) => void store.setSessionApprovalMode(session.session_id, mode)}
                  open={open_picker === "approval"}
                  options={APPROVAL_OPTIONS}
                  selected={session.approval_mode}
                  trigger_class_name={styles.compact_selector}
                />
                <span className={styles.action_spacer} />
                <ContextUsageRing view={session_view} />
                {model_options.length > 0 && (
                  <SelectionPopover
                    aria_label="切换会话模型"
                    disabled={!is_idle_for_model || store.composer_pending}
                    on_open_change={(open) => setOpenPicker(open ? "model" : null)}
                    on_select={(model_key) => void store.setSessionModel(session.session_id, model_key)}
                    open={open_picker === "model"}
                    options={model_options}
                    selected={session.model_key}
                    title="选择模型"
                    trigger_class_name={styles.model_selector}
                    trigger_content={modelDisplayName(application, session.model_key)}
                  />
                )}
                <button
                  aria-label={draft.trim() ? "发送消息" : session.active_run_id ? "中断当前轮次" : "发送消息"}
                  className={styles.send_button}
                  data-action={!draft.trim() && session.active_run_id ? "interrupt" : "send"}
                  disabled={store.composer_pending || attachment_flow.pending || (!draft.trim() && !session.active_run_id)}
                  onClick={() => draft.trim()
                    ? void submitDraft()
                    : session.active_run_id && void store.interruptRun(session.session_id, session.active_run_id)}
                  type="button"
                >
                  <Icon name={!draft.trim() && session.active_run_id ? "stop" : "arrow-down"} size={16} />
                </button>
              </footer>
            </section>
          )}
        </>
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
