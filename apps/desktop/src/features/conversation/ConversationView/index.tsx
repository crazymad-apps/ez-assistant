import { childKey } from "../../../stores/liveExecutionProjection";
import { revealFileLabel } from "../../../platform/fileManager";
import { observer } from "mobx-react-lite";
import { McpControlResult } from "../McpControlResult";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type {
  AttachmentSummary,
  ConversationOwner,
  MessageId,
  QuotedTextSnapshot,
  ToolFileReference,
  ToolCallId,
  UserMessageSnapshot,
} from "@ez-assistant/protocol";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { Collapse } from "../../../components/Collapse";
import { PresenceBoundary, usePresence } from "../../../components/Presence";
import type { LiveToolSnapshot } from "../../../stores/LiveExecutionStore";
import { useRootStore } from "../../../stores/RootStoreContext";
import { sameConversationOwner } from "../../../stores/ConversationNavigationStore";
import { ToolDetailDialog, type ToolDetailView } from "../ToolDetailDialog";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { AttachmentPreviewDialog } from "../../context-panel/AttachmentPreviewDialog";
import { mergeChildTaskItems } from "../childTaskPresentation";
import { attachLiveTurn, groupConversationTurns, type ConversationRow } from "./conversationRows";
import { EmptyConversation, UserMessage } from "./MessageViews";
import { ProcessingTurn } from "./ProcessingTurn";
import { ConversationNavigation } from "./ConversationNavigation";
import { MessageCards } from '../../message-cards';
import { createQuotedTextSnapshot, quoteSourceRange } from "../quoteSelection";
import { ConversationMarkdownContent } from "../../resource-workspace/ConversationMarkdownContent";
import { isPreviewableResource } from "../../resource-workspace/ResourceWorkspaceStore";
import {
  openAttachmentInSystem,
  revealAttachmentInDirectory,
} from "../../../native-bridge/nativeResource";
import {
  ResourceContextMenu,
  type ResourceMenuLocation,
} from "../../resource-workspace/ResourceContextMenu";
import styles from "./index.module.scss";
import { useOutputFollow } from "./useOutputFollow";

const BOTTOM_THRESHOLD = 72;

export const ConversationView = observer(function ConversationView({ navigation_slot }: Readonly<{
  navigation_slot?: HTMLDivElement | null;
}>) {
  const store = useRootStore();
  const session_id = store.navigation.selected_session_id;
  const child_task_id = store.navigation.selected_child_task_id;
  const history = child_task_id && session_id
    ? store.projection.child_conversation_histories.get(childKey(session_id, child_task_id))
    : session_id ? store.projection.conversation_histories.get(session_id) : undefined;
  const child_view = child_task_id && session_id ? store.projection.child_task_views.get(childKey(session_id, child_task_id)) : undefined;
  const receipts = !child_task_id && session_id ? store.projection.input_receipts.get(session_id) : undefined;
  const load_state = session_id ? store.projection.conversationLoadState(session_id, child_task_id) : undefined;
  const load_failure = load_state?.status === "error" ? (
    <div role="alert" className={styles.load_error}>
      {load_state.error}
      <button onClick={() => { void store.reloadSelectedConversation(); }} type="button">重试</button>
    </div>
  ) : null;
  const live_run = child_task_id && session_id
    ? store.live_execution.runForChildTask(session_id, child_task_id)
    : session_id ? store.live_execution.runForSession(session_id) : null;
  const scroll_ref = useRef<HTMLDivElement>(null);
  const message_list_ref = useRef<HTMLDivElement>(null);
  const quote_scroll_top = useRef<number | null>(null);
  const [show_scroll_bottom, setShowScrollBottom] = useState(false);
  const [locate_revision, setLocateRevision] = useState(0);
  const [located, setLocated] = useState<{ id: MessageId; scope: number } | null>(null);
  const navigation_scope = store.conversation_navigation.scope_revision;
  const follow = useOutputFollow({
    scroll: scroll_ref,
    content: message_list_ref,
    identity: `${session_id}:${child_task_id}:${store.navigation.conversation_history_index}:${history?.generation}`,
    active: Boolean(history && (history.items.length > 0 || live_run || child_view)),
    restore: () => {
      const location = store.navigation.current_conversation_location;
      if (location?.session_id !== session_id || location.child_task_id !== child_task_id) return undefined;
      if (location.reading_generation === history?.generation) return location.reading;
      return location.reading?.anchor ? { ...location.reading, top: 0 } : undefined;
    },
    recoverAnchor: (message_id) => session_id
      ? store.restoreReadingAnchor(session_id, child_task_id, message_id) : Promise.resolve(false),
    onUserScroll: node => { if (node.scrollTop < 160) void loadPrevious(); },
    save: (position) => {
      const location = store.navigation.current_conversation_location;
      if (history && location?.session_id === session_id && location.child_task_id === child_task_id) {
        store.navigation.updateReadingPosition(position, history.generation);
      }
      const node = scroll_ref.current;
      setShowScrollBottom(Boolean(node && node.scrollHeight - node.scrollTop - node.clientHeight > BOTTOM_THRESHOLD));
    },
  });
  const is_pinned_to_bottom = follow.following;
  const [detail_state, setDetailState] = useState<{
    detail: ToolDetailView | null;
    error: string | null;
    is_loading: boolean;
  } | null>(null);
  const [fork_point, setForkPoint] = useState<MessageId | null>(null);
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const [attachment_menu, setAttachmentMenu] = useState<Readonly<{
    attachment: AttachmentSummary;
    location: ResourceMenuLocation;
  }> | null>(null);
  const [summary_expanded, setSummaryExpanded] = useState(false);
  const [selection_action, setSelectionAction] = useState<{
    quote: QuotedTextSnapshot;
    x: number;
    y: number;
  } | null>(null);
  const quote_bubble_ref = useRef<HTMLDivElement>(null);
  const selection_presence = usePresence(selection_action !== null, 90);
  const retained_selection_action_ref = useRef(selection_action);
  if (selection_action) retained_selection_action_ref.current = selection_action;
  const conversation_rows = useMemo(
    () => {
      const items = history?.items ?? [];
      const visible = receipts ? [...items, ...receipts.map(receipt => receipt.message)] : items;
      return attachLiveTurn(groupConversationTurns(visible, child_task_id !== null), live_run, child_task_id);
    },
    [child_task_id, history?.items, live_run, receipts],
  );

  let owner: ConversationOwner | null = history?.owner ?? null;
  if (!owner && session_id) owner = child_task_id
    ? { type: "child_task", session_id, child_task_id }
    : { type: "main_session", session_id };
  let reveal_message_id = store.navigation.conversation_anchor_message_id;
  const focus_target = store.transient_focus.target;
  if (focus_target && owner && sameConversationOwner(focus_target.owner, owner)
    && focus_target.generation === history?.generation) reveal_message_id = focus_target.message_id;
  const session = store.projection.sessionSummary(session_id);
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const editor = store.history_edit;
  const edit_target = !child_task_id ? editor.draft?.message_id : undefined;
  const active_session_run = session_id && store.live_execution.activeRunId(session_id,
    receipts?.at(-1)?.run.run_id ?? session_view?.session.active_run_id ?? session?.active_run_id ?? null);
  const show_edit = !active_session_run;
  const switch_target = editor.switchTarget;
  const child_task_items = session_id
    ? mergeChildTaskItems(
        session_view?.child_tasks ?? [],
        store.live_execution.childTasksForSession(session_id),
      )
    : [];
  const attachment_by_id = useMemo(() => new Map(
    (session_view?.attachments ?? []).map((attachment) => [attachment.attachment_id, attachment]),
  ), [session_view?.attachments]);

  const openAttachment = useCallback((attachment: AttachmentSummary) => {
    if (!session_id || !isPreviewableResource(attachment.original_name, attachment.media_type)) {
      setPreviewAttachment(attachment);
      return;
    }
    store.resource_workspace.openAttachment(
      `session:${session_id}`,
      attachment,
      session_view?.attachments ?? [attachment],
    );
    if (!store.navigation.effective_right_sidebar_open) store.toggleRightSidebar();
  }, [session_id, session_view?.attachments, store]);

  const runAttachmentAction = useCallback((request: Promise<void>, fallback: string) => {
    void request.catch((failure: unknown) => {
      store.showInteractionError(failure instanceof Error ? failure.message : fallback);
    });
  }, [store]);

  const confirmFork = useCallback(async () => {
    if (!session_id || !fork_point || !history) {
      return;
    }
    const created = await store.forkSession(session_id, fork_point, history.generation);
    if (created) {
      setForkPoint(null);
    }
  }, [fork_point, history, session_id, store]);

  const loadPrevious = useCallback(async (retry = false) => {
    if (!session_id || !history?.has_more || history.is_loading_previous || (!retry && history.load_error)) return;
    follow.remember();
    await store.loadPreviousConversationPage(session_id, child_task_id);
  }, [child_task_id, history?.has_more, history?.is_loading_previous, history?.load_error, session_id, store, follow.remember]);

  const updateScrollState = useCallback((node: HTMLDivElement) => {
    setShowScrollBottom(node.scrollHeight - node.scrollTop - node.clientHeight > BOTTOM_THRESHOLD);
  }, []);

  useLayoutEffect(() => {
    const message_id = store.navigation.conversation_anchor_message_id;
    const node = scroll_ref.current;
    if (!message_id || !node) {
      return;
    }
    const target = node.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(message_id)}"]`);
    if (target) {
      const quote_target = store.transient_focus.target;
      const quote_will_position_range = quote_target?.message_id === message_id
        && quote_target.generation === history?.generation
        && owner !== null
        && sameConversationOwner(quote_target.owner, owner);
      if (store.navigation.conversation_anchor_follows_output) {
        follow.bottom();
      } else if (!quote_will_position_range) {
        is_pinned_to_bottom.current = false;
        target.scrollIntoView({ block: "center" });
        follow.remember();
        store.navigation.updateCurrentScrollOffset(node.scrollTop);
      }
      store.navigation.consumeConversationAnchor(message_id);
    }
  }, [
    history?.generation,
    history?.items,
    receipts,
    owner,
    store.navigation,
    store.navigation.conversation_anchor_message_id,
    store.transient_focus.target,
  ]);

  const handleScroll = useCallback(() => {
    const node = scroll_ref.current;
    if (!node) {
      return;
    }
    const expected_quote_scroll_top = quote_scroll_top.current;
    quote_scroll_top.current = null;
    const is_quote_positioning_scroll = (
      expected_quote_scroll_top !== null
      && Math.abs(node.scrollTop - expected_quote_scroll_top) < 1
    );
    store.navigation.updateCurrentScrollOffset(node.scrollTop);
    updateScrollState(node);
    if (is_quote_positioning_scroll) {
      return;
    }
    setSelectionAction(null);
    store.transient_focus.clear();
  }, [store.navigation, updateScrollState]);

  const captureSelection = useCallback(() => {
    if (!session_id || !owner || !history) return;
    const selection = window.getSelection();
    const range = selection && selection.rangeCount > 0 ? selection.getRangeAt(0) : null;
    if (!selection || selection.isCollapsed || !range) {
      setSelectionAction(null);
      return;
    }
    const start = quoteRootContainer(range.startContainer);
    const end = quoteRootContainer(range.endContainer);
    if (!start || start !== end || !scroll_ref.current?.contains(start)) {
      setSelectionAction(null);
      return;
    }
    const boundaryElement = (node: Node) => node instanceof Element ? node : node.parentElement;
    const article = boundaryElement(range.startContainer)?.closest<HTMLElement>("[data-message-id]");
    const end_article = boundaryElement(range.endContainer)?.closest<HTMLElement>("[data-message-id]");
    if (article !== end_article && article?.dataset.messageId !== end_article?.dataset.messageId) {
      setSelectionAction(null);
      return;
    }
    const message_id = article?.dataset.messageId;
    const source_role = start.dataset.quoteRole;
    if (!message_id || (source_role !== "user" && source_role !== "assistant")) {
      setSelectionAction(null);
      return;
    }
    const source_item = history.items.find((item) => item.message_id === message_id);
    const quote = createQuotedTextSnapshot(start, range.cloneRange(), {
      quote_id: createQuoteId(),
      source_owner: owner,
      source_generation: history.generation,
      source_message_id: message_id,
      source_role,
      source_label: child_view?.task.task.title ?? session?.title ?? "当前会话",
      source_created_at_ms: source_item?.type === "user"
        ? source_item.created_at_ms
        : source_item?.type === "assistant"
          ? source_item.finished_at_ms ?? source_item.created_at_ms
          : null,
    });
    if (!quote) {
      setSelectionAction(null);
      return;
    }
    const rect = range.getBoundingClientRect();
    setSelectionAction({
      quote,
      x: rect.left + rect.width / 2,
      y: rect.top - 8,
    });
  }, [child_view?.task.task.title, history, owner, session?.title, session_id]);

  const submitSelectionQuote = useCallback(() => {
    if (!selection_action) return;
    if (store.history_edit.draft && store.history_edit.draft.phase !== "editing") {
      store.showInteractionError("请等待修改的发送结果。");
      return;
    }
    const quote = selection_action.quote;
    setSelectionAction(null);
    window.getSelection()?.removeAllRanges();
    if (!store.history_edit.addQuote(quote)) store.composer_quotes.add(session_id!, quote);
    store.transient_focus.clear();
    if (store.navigation.selected_child_task_id) store.closeChildTask();
  }, [selection_action, session_id, store]);

  useEffect(() => {
    const ownsSelection = (selection: Selection) => (
      scroll_ref.current?.contains(selection.anchorNode) || scroll_ref.current?.contains(selection.focusNode)
    );
    function dismissSelection(event: PointerEvent) {
      // 保留右键复制、Shift 扩选和引用按钮；普通空白/控件点击结束会话选区。
      if (event.button !== 0 || event.ctrlKey || event.shiftKey) return;
      const target = event.target;
      if (!(target instanceof Element) || quote_bubble_ref.current?.contains(target)) return;
      setSelectionAction(null);
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || !ownsSelection(selection)) return;
      const style = getComputedStyle(target);
      // user-select:none 不会触发浏览器原生的选区折叠，必须在新手势开始前清除旧锚点。
      if (!scroll_ref.current?.contains(target)
        || style.userSelect === "none"
        || style.getPropertyValue("-webkit-user-select") === "none") {
        selection.removeAllRanges();
      }
    }
    function dismissCollapsedSelection() {
      const selection = window.getSelection();
      if (!selection || selection.isCollapsed || !ownsSelection(selection)) {
        setSelectionAction(null);
      }
    }
    document.addEventListener("pointerdown", dismissSelection, true);
    document.addEventListener("selectionchange", dismissCollapsedSelection);
    return () => {
      document.removeEventListener("pointerdown", dismissSelection, true);
      document.removeEventListener("selectionchange", dismissCollapsedSelection);
      const selection = window.getSelection();
      if (selection && ownsSelection(selection)) selection.removeAllRanges();
      setSelectionAction(null);
    };
  }, [session_id, child_task_id]);

  useEffect(() => {
    if (store.composer_pending) {
      setSelectionAction(null);
      window.getSelection()?.removeAllRanges();
    }
  }, [store.composer_pending]);

  useLayoutEffect(() => {
    const target = store.transient_focus.target;
    const node = scroll_ref.current;
    if (!target || !node || !owner || !sameConversationOwner(target.owner, owner)) return;
    if (target.generation !== history?.generation) {
      store.transient_focus.clear();
      return;
    }
    const escaped_message_id = CSS.escape(target.message_id);
    const message = node.querySelector<HTMLElement>(`[data-message-id="${escaped_message_id}"]`);
    if (!message) {
      store.transient_focus.clear();
      return;
    }
    const root = node.querySelector<HTMLElement>(
      `[data-quote-root='true'][data-message-id="${escaped_message_id}"]`,
    ) ?? (message.matches("[data-quote-root='true']")
      ? message
      : message.closest<HTMLElement>("[data-quote-root='true']") ?? message.querySelector<HTMLElement>("[data-quote-root='true']"));
    const range = root ? quoteSourceRange(root, target, target.message_id) : null;
    if (!root || !range) {
      message.scrollIntoView({ block: "center" });
      store.transient_focus.clear();
      return;
    }
    const target_scroll_top = centeredRangeScrollTop(node, range);
    if (target_scroll_top === null) {
      message.scrollIntoView({ block: "center" });
    } else {
      quote_scroll_top.current = target_scroll_top;
      is_pinned_to_bottom.current = false;
      node.scrollTo({ top: target_scroll_top, behavior: "auto" });
      follow.remember();
      store.navigation.updateCurrentScrollOffset(node.scrollTop);
      updateScrollState(node);
    }
    return applyTransientQuoteHighlight(root, range);
  }, [
    history?.generation,
    history?.items,
    owner,
    store.navigation,
    store.transient_focus,
    store.transient_focus.target?.nonce,
    updateScrollState,
  ]);

  useEffect(() => () => store.transient_focus.clear(), [store.transient_focus]);

  const scrollToBottom = () => {
    store.cancelConversationLocation();
    setLocateRevision(value => value + 1);
    follow.bottom();
  };

  async function locateNavigationMessage(
    id: MessageId, on_error: (message: string) => void, current: () => boolean,
  ): Promise<boolean> {
    if (!history) return false;
    follow.pause();
    if (!await store.locateConversationUser(history.owner, history.generation, id, on_error) || !current()) return false;
    setLocated({ id, scope: navigation_scope });
    return true;
  }

  useLayoutEffect(() => {
    const node = scroll_ref.current;
    if (!node || !located || located.scope !== navigation_scope) return;
    const target = node.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(located.id)}"]`);
    if (!target) return;
    node.scrollTop += target.getBoundingClientRect().top - node.getBoundingClientRect().top - 32;
    quote_scroll_top.current = node.scrollTop;
    follow.pause();
    store.navigation.updateCurrentScrollOffset(node.scrollTop);
    const timer = setTimeout(() => setLocated(null), 1200);
    return () => clearTimeout(timer);
  }, [located, navigation_scope, follow.pause, store.navigation]);

  const openToolDetail = useCallback(async (message_id: MessageId, call_id: ToolCallId) => {
    if (!owner) {
      return;
    }
    setDetailState({ detail: null, error: null, is_loading: true });
    try {
      const detail = await store.getToolDetail(owner, message_id, call_id);
      setDetailState({ detail, error: null, is_loading: false });
    } catch (error: unknown) {
      setDetailState({
        detail: null,
        error: error instanceof Error ? error.message : "无法读取工具详情。",
        is_loading: false,
      });
    }
  }, [owner, store]);

  const openLiveToolDetail = useCallback((tool: LiveToolSnapshot) => {
    setDetailState({
      detail: {
        source: "live",
        tool_name: tool.tool_name,
        mcp_identity: tool.mcp_identity,
        status: tool.status,
        input: tool.input,
        request_json: null,
        result_summary: null,
        result_json: null,
        recall: null,
        stdout: tool.stdout.trim() || null,
        stderr: tool.stderr.trim() || null,
        error: null,
        files: [],
        output_truncated: false,
        historical_fields_missing: true,
      },
      error: null,
      is_loading: false,
    });
  }, []);

  const openToolResource = useCallback((file: ToolFileReference) => {
    const detail = detail_state?.detail;
    if (!session_id || !detail?.owner || !detail.message_id) return;
    store.resource_workspace.openToolResource(
      `session:${session_id}`,
      detail.owner,
      detail.message_id,
      file,
      detail.files,
    );
    if (!store.navigation.effective_right_sidebar_open) store.toggleRightSidebar();
    setDetailState(null);
  }, [detail_state?.detail, session_id, store]);

  useEffect(() => {
    setSummaryExpanded(false);
    setAttachmentMenu(null);
  }, [history?.generation, session_id]);

  const renderConversationRows = (rows: readonly ConversationRow[]) => (
    rows.map((row, row_index) => {
      if (row.type === "control_result") {
        return <McpControlResult key={row.message.message_id} message={row.message} />;
      }
      return row.type === "context_summary"
      ? (
          <div className={styles.context_summary_boundary} key={row.message.message_id}>
            <span className={styles.context_summary_line} />
            <button
              aria-expanded={summary_expanded}
              onClick={() => setSummaryExpanded((current) => !current)}
              type="button"
            >
              {summary_expanded ? "收起上下文摘要" : "查看上下文摘要"}
            </button>
            <span className={styles.context_summary_line} />
            <div className={styles.context_summary_collapse}>
              <Collapse open={summary_expanded}>
                <div className={styles.context_summary_text}>
                  <ConversationMarkdownContent text={row.message.text} />
                </div>
              </Collapse>
            </div>
          </div>
        )
      : row.type === "user"
      ? (
          <UserMessage
            attachments={(row.message.attachment_ids ?? []).flatMap((id) => {
              const attachment = attachment_by_id.get(id);
              return attachment ? [attachment] : [];
            })}
            key={row.message.message_id}
            editing={edit_target === row.message.message_id}
            edit={show_edit ? {
              loading: editor.busyTarget === row.message.message_id,
              disabled_reason: editReason(row.message),
              on_select: () => void editor.begin(row.message.message_id),
            } : undefined}
            navigation_highlight={located?.scope === navigation_scope && located.id === row.message.message_id}
            message={row.message}
            on_attachment_click={openAttachment}
            on_attachment_menu={(attachment, location) => setAttachmentMenu({ attachment, location })}
            on_quote_locate={(quote) => session_id
              ? store.locateTextQuoteSource(session_id, quote)
              : Promise.resolve(false)}
            on_source_open={(source_session) => {
              store.navigation.setListMode(source_session.lifecycle === "archived" ? "archived" : "active");
              void store.selectSession(source_session.session_id);
            }}
            source_session={store.projection.sessionSummary(sourceSessionId(row.message.source))}
          />
        )
      : (
          <div className={styles.turn_with_tasks} key={`${history?.generation}:${row.key}`}>
            <ProcessingTurn
              cards={owner && !rows.slice(row_index + 1).some((next) =>
                next.type === 'assistant_turn' && (child_task_id !== null || (row.run_id !== null && next.run_id === row.run_id)))
                ? <MessageCards owner={owner} run_id={row.run_id} message_ids={row.messages.map(message => message.message_id)} />
                : null}
              child_tasks={!child_task_id && row.run_id
                ? child_task_items.filter((item) => item.task.parent_run_id === row.run_id)
                : []}
              live_run={row.live_run ?? null}
              task={child_task_id ? child_view?.task.task : undefined}
              reveal_message_id={reveal_message_id}
              awaiting_approval={child_task_id ? Boolean(child_view?.approval_ids.length) : session_view?.approvals?.items.some((approval) => approval.run_id === row.run_id)}
              awaiting_feedback={!child_task_id && !!session_id && store.feedback.waiting(session_id)?.run_id === row.run_id}
              messages={row.messages}
              on_child_open={!child_task_id && session_id
                ? (item) => void store.openChildTask(session_id, item.task.child_task_id)
                : undefined}
              onFork={setForkPoint}
              show_fork={session?.role !== "controller"}
              onLiveToolClick={openLiveToolDetail}
              onToolClick={openToolDetail}
            />
          </div>
        ); })
  );

  function editReason(message: UserMessageSnapshot): string | undefined {
    if (child_task_id) return "子任务只读，无法编辑";
    if (!store.projection.application?.capabilities.user_message_edit) return "当前 Host 不支持消息编辑";
    if (session_view?.session.lifecycle === "archived" || session?.lifecycle === "archived") return "归档会话不可编辑";
    if (message.inherited) return "继承消息不可编辑";
    if (!message.input_id || message.source?.type === "controller_delivery" || message.source?.type === "proxy_report") {
      return "这条消息不能编辑";
    }
    if (store.connection.state !== "connected" || store.projection.is_stale) return "Runtime 未连接";
    if (editor.draft && editor.draft.phase !== "editing") return "请等待发送结果";
    if (editor.busyTarget && editor.busyTarget !== message.message_id) return "正在读取原消息";
    if (editor.sessionBusy) return "会话当前无法编辑历史消息";
    return undefined;
  }

  if (!session_id) {
    if (store.navigation.selected_draft_key) {
      return <EmptyConversation title="开始新会话" detail="输入消息或使用 / 指令。" />;
    }
    return <EmptyConversation title="选择一个会话" detail="从左侧项目中选择会话以查看消息。" />;
  }
  if (!history) {
    if (child_task_id) {
      return (
        <>
          <div className={styles.viewport}>
            <div className={styles.scroll}>
              <div className={styles.message_list}>
                {live_run
                  ? <>{load_failure}<ProcessingTurn messages={[]} live_run={live_run} child_tasks={[]} onFork={setForkPoint} show_fork={false} onLiveToolClick={openLiveToolDetail} onToolClick={openToolDetail} /></>
                  : <EmptyConversation title={load_state?.error ? "读取子任务失败" : "正在读取子任务"}
                    detail={load_state?.error ?? "正在从运行时获取可靠消息投影。"}
                    on_retry={load_state?.error ? () => { void store.reloadSelectedConversation(); } : undefined} />}
              </div>
            </div>
          </div>
          <PresenceBoundary present={detail_state !== null}>
          {detail_state && (
            <ToolDetailDialog
              detail={detail_state.detail}
              error={detail_state.error}
              is_loading={detail_state.is_loading}
              on_close={() => setDetailState(null)}
              on_file_open={openToolResource}
              on_recall_navigate={(target) => {
                setDetailState(null);
                void store.openRecallNavigationTarget(target);
              }}
            />
          )}
          </PresenceBoundary>
        </>
      );
    }
    return <EmptyConversation
      title={load_state?.error ? "读取会话失败" : "正在读取会话"}
      detail={load_state?.error ?? "正在从运行时获取可靠消息投影。"}
      on_retry={load_state?.error && session_id ? () => { void store.reloadSelectedConversation(); } : undefined}
    />;
  }
  if (load_state?.status === "error" && history.items.length === 0 && !live_run) {
    return <EmptyConversation title="读取会话失败" detail={load_state.error ?? "请重试。"} on_retry={() => { void store.reloadSelectedConversation(); }} />;
  }
  if (history.items.length === 0 && !live_run) {
    if (!child_view) {
      return store.remote_device
        ? <EmptyConversation title="会话暂无消息" detail="当前正在查看远端会话。" />
        : <EmptyConversation title="开始一段新会话" detail="从下方输入消息开始对话。" />;
    }
  }

  return (
    <div className={styles.viewport}>
      <div
        aria-label="消息列表"
        tabIndex={0}
        data-output-scroll="true"
        className={styles.scroll}
        onMouseUp={(event) => {
          if (event.button === 0 && !event.ctrlKey) captureSelection();
        }}
        onScroll={handleScroll}
        ref={scroll_ref}
      >
        <div className={styles.message_list} ref={message_list_ref}>
          {load_failure}
          {child_view && history.items.length === 0 && !live_run && (
            <EmptyConversation title={child_view.task.task.title} detail="子智能体尚未产生可展示的消息。" />
          )}
          {history.has_more && (
            <button className={styles.load_previous} disabled={history.is_loading_previous} onClick={() => void loadPrevious(true)} type="button">
              {history.is_loading_previous ? "正在加载更早消息…" : history.load_error ? "重试" : "加载更早消息"}
            </button>
          )}
          {history.load_error && <p className={styles.load_error}>{history.load_error}</p>}
          {renderConversationRows(conversation_rows)}
        </div>
      </div>
      {navigation_slot && createPortal(
        <ConversationNavigation
          key={navigation_scope}
          store={store.conversation_navigation}
          layout={navigation_slot}
          content_ref={message_list_ref}
          scroll_ref={scroll_ref}
          locate={locateNavigationMessage}
          locate_revision={locate_revision}
          cancel_location={() => store.cancelConversationLocation()}
        />,
        navigation_slot,
      )}
      {selection_presence.mounted && retained_selection_action_ref.current && (
        <div
          aria-hidden={selection_presence.state === "exiting" ? true : undefined}
          className={styles.quote_bubble}
          data-presence={selection_presence.state}
          inert={selection_presence.state === "exiting" ? true : undefined}
          onTransitionEnd={selection_presence.onTransitionEnd}
          ref={quote_bubble_ref}
          role="toolbar"
          style={{ left: retained_selection_action_ref.current.x, top: retained_selection_action_ref.current.y }}
        >
          <button onClick={submitSelectionQuote} type="button">
            <Icon name="quote" size={14} />引用
          </button>
        </div>
      )}
      {show_scroll_bottom && (
        <Tooltip content="返回最新">
        <button
          aria-label="返回最新"
          className={styles.scroll_bottom}
          onClick={scrollToBottom}
          type="button"
        >
          <Icon name="arrow-down" size={16} />
        </button>
        </Tooltip>
      )}
      {attachment_menu && (
        <ResourceContextMenu
          items={[
            {
              disabled: !isPreviewableResource(
                attachment_menu.attachment.original_name,
                attachment_menu.attachment.media_type,
              ),
              label: "在资源栏打开",
              on_select: () => openAttachment(attachment_menu.attachment),
            },
            {label:"下载文件",on_select:()=>runAttachmentAction(store.files.downloadAttachment(attachment_menu.attachment.session_id, attachment_menu.attachment.attachment_id, attachment_menu.attachment.original_name),"下载失败。")},
            ...(store.files.native_host ? [
            {
              label: "使用系统应用打开",
              on_select: () => runAttachmentAction(
                openAttachmentInSystem(
                  attachment_menu.attachment.session_id,
                  attachment_menu.attachment.attachment_id,
                ),
                "无法使用系统应用打开。",
              ),
            },
            {
              label: revealFileLabel(),
              on_select: () => runAttachmentAction(
                revealAttachmentInDirectory(
                  attachment_menu.attachment.session_id,
                  attachment_menu.attachment.attachment_id,
                ),
                "无法在文件管理器中显示。",
              ),
            }            ] : []),
          ]}
          location={attachment_menu.location}
          on_close={() => setAttachmentMenu(null)}
        />
      )}
      <PresenceBoundary present={detail_state !== null}>
      {detail_state && (
        <ToolDetailDialog
          detail={detail_state.detail}
          error={detail_state.error}
          is_loading={detail_state.is_loading}
          on_close={() => setDetailState(null)}
          on_file_open={openToolResource}
          on_recall_navigate={(target) => {
            setDetailState(null);
            void store.openRecallNavigationTarget(target);
          }}
        />
      )}
      </PresenceBoundary>
      <PresenceBoundary present={fork_point !== null}>
      {fork_point && (
        <SessionActionDialog
          confirm_label="创建分支"
          is_pending={store.pending_session_action}
          on_cancel={() => setForkPoint(null)}
          on_confirm={() => void confirmFork()}
          title="从这条回复创建分支？"
        >
          <p>将复制 <strong>{session?.title ?? "当前会话"}</strong> 到所选助手回复，并创建一段独立会话。</p>
          <p>后续消息、运行记录和工作目录中的用户文件不会被复制或修改。</p>
        </SessionActionDialog>
      )}
      </PresenceBoundary>
      {switch_target && !child_task_id && (
        <SessionActionDialog
          cancel_label="继续当前编辑"
          confirm_label="放弃修改并切换"
          is_pending={false}
          on_cancel={() => editor.dismissSwitch()}
          on_confirm={() => void editor.begin(switch_target, true)}
          title="切换编辑的消息？"
        >
          <p>切换后，当前编辑稿中的修改会丢失。</p>
        </SessionActionDialog>
      )}
      <PresenceBoundary present={preview_attachment !== null}>
      {preview_attachment && (
        <AttachmentPreviewDialog
          attachment={preview_attachment}
          on_close={() => setPreviewAttachment(null)}
        />
      )}
      </PresenceBoundary>
    </div>
  );
});

function sourceSessionId(
  source: import("@ez-assistant/protocol").ConversationInputSourceSnapshot | undefined,
): string | null {
  if (!source) return null;
  if (source.type === "controller_delivery") return source.controller_session_id;
  if (source.type === "proxy_report") return source.source_session_id;
  return null;
}

function quoteRootContainer(node: Node): HTMLElement | null {
  const element = node instanceof HTMLElement ? node : node.parentElement;
  return element?.closest<HTMLElement>("[data-quote-root='true']") ?? null;
}

function createQuoteId(): string {
  return typeof crypto.randomUUID === "function"
    ? `quote-${crypto.randomUUID()}`
    : `quote-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function applyTransientQuoteHighlight(root: HTMLElement, range: Range): () => void {
  const registry = (globalThis.CSS as unknown as {
    highlights?: { set(name: string, highlight: unknown): void; delete(name: string): boolean };
  } | undefined)?.highlights;
  const HighlightConstructor = (globalThis as unknown as {
    Highlight?: new (...ranges: Range[]) => unknown;
  }).Highlight;
  if (registry && HighlightConstructor) {
    registry.delete("quote-source");
    registry.set("quote-source", new HighlightConstructor(range));
    return () => { registry.delete("quote-source"); };
  }
  root.dataset.transientFocus = "true";
  return () => { delete root.dataset.transientFocus; };
}

function centeredRangeScrollTop(container: HTMLElement, range: Range): number | null {
  const range_rect = range.getBoundingClientRect();
  const container_rect = container.getBoundingClientRect();
  if (
    container.clientHeight <= 0
    || range_rect.height <= 0
    || !Number.isFinite(range_rect.top)
    || !Number.isFinite(range_rect.height)
    || !Number.isFinite(container_rect.top)
  ) {
    return null;
  }
  const range_center = range_rect.top + range_rect.height / 2;
  const container_center = container_rect.top + container.clientHeight / 2;
  const unclamped = container.scrollTop + range_center - container_center;
  return Math.min(
    Math.max(0, container.scrollHeight - container.clientHeight),
    Math.max(0, unclamped),
  );
}
