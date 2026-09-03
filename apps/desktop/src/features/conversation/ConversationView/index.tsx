import { observer } from "mobx-react-lite";
import { McpControlResult } from "../McpControlResult";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type {
  AttachmentSummary,
  ConversationOwner,
  MessageId,
  QuotedTextSnapshot,
  ToolCallId,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { MarkdownContent } from "../../../components/MarkdownContent";
import { Collapse } from "../../../components/Collapse";
import { PresenceBoundary, usePresence } from "../../../components/Presence";
import type { LiveToolSnapshot } from "../../../stores/LiveExecutionStore";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ToolDetailDialog, type ToolDetailView } from "../ToolDetailDialog";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { AttachmentPreviewDialog } from "../../context-panel/AttachmentPreviewDialog";
import { mergeChildTaskItems } from "../childTaskPresentation";
import { groupConversationTurns, type ConversationRow } from "./conversationRows";
import { AssistantTurn, EmptyConversation, LiveAssistantMessage, UserMessage } from "./MessageViews";
import { createQuotedTextSnapshot, quoteSourceRange } from "../quoteSelection";
import styles from "./index.module.scss";

const BOTTOM_THRESHOLD = 72;
const PINNED_BOTTOM_THRESHOLD = 24;

export const ConversationView = observer(function ConversationView() {
  const store = useRootStore();
  const session_id = store.navigation.selected_session_id;
  const child_task_id = store.navigation.selected_child_task_id;
  const history = child_task_id
    ? store.projection.child_conversation_histories.get(child_task_id)
    : session_id ? store.projection.conversation_histories.get(session_id) : undefined;
  const child_view = child_task_id ? store.projection.child_task_views.get(child_task_id) : undefined;
  const live_run = child_task_id
    ? store.live_execution.runForChildTask(child_task_id)
    : session_id ? store.live_execution.runForSession(session_id) : null;
  const scroll_ref = useRef<HTMLDivElement>(null);
  const message_list_ref = useRef<HTMLDivElement>(null);
  const previous_scroll_height = useRef<number | null>(null);
  const quote_scroll_top = useRef<number | null>(null);
  const is_pinned_to_bottom = useRef(true);
  const [show_scroll_bottom, setShowScrollBottom] = useState(false);
  const [detail_state, setDetailState] = useState<{
    detail: ToolDetailView | null;
    error: string | null;
    is_loading: boolean;
  } | null>(null);
  const [fork_point, setForkPoint] = useState<MessageId | null>(null);
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const [summary_expanded, setSummaryExpanded] = useState(false);
  const [selection_action, setSelectionAction] = useState<{
    quote: QuotedTextSnapshot;
    x: number;
    y: number;
  } | null>(null);
  const selection_presence = usePresence(selection_action !== null, 90);
  const retained_selection_action_ref = useRef(selection_action);
  if (selection_action) retained_selection_action_ref.current = selection_action;
  const conversation_rows = useMemo(
    () => groupConversationTurns(history?.items ?? [], child_task_id !== null),
    [child_task_id, history?.items],
  );

  const owner: ConversationOwner | null = history?.owner ?? (session_id
    ? { type: "main_session", session_id }
    : null);
  const session = store.projection.application?.active_sessions.find((item) => item.session_id === session_id)
    ?? store.projection.application?.archived_sessions.find((item) => item.session_id === session_id);
  const all_sessions = [
    ...(store.projection.application?.active_sessions ?? []),
    ...(store.projection.application?.archived_sessions ?? []),
  ];
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const child_task_items = session_id
    ? mergeChildTaskItems(
        session_view?.child_tasks ?? [],
        store.live_execution.childTasksForSession(session_id),
        (task_id) => store.live_execution.runForChildTask(task_id),
      )
    : [];
  const attachment_by_id = useMemo(() => new Map(
    (session_view?.attachments ?? []).map((attachment) => [attachment.attachment_id, attachment]),
  ), [session_view?.attachments]);

  const confirmFork = useCallback(async () => {
    if (!session_id || !fork_point || !history) {
      return;
    }
    const created = await store.forkSession(session_id, fork_point, history.generation);
    if (created) {
      setForkPoint(null);
    }
  }, [fork_point, history, session_id, store]);

  const loadPrevious = useCallback(async () => {
    const node = scroll_ref.current;
    if (!session_id || !node || !history?.has_more || history.is_loading_previous) {
      return;
    }
    previous_scroll_height.current = node.scrollHeight;
    const loaded = await store.loadPreviousConversationPage(session_id, child_task_id);
    if (!loaded) {
      previous_scroll_height.current = null;
    }
  }, [child_task_id, history?.has_more, history?.is_loading_previous, session_id, store]);

  useLayoutEffect(() => {
    const node = scroll_ref.current;
    if (!node || previous_scroll_height.current === null) {
      return;
    }
    node.scrollTop += node.scrollHeight - previous_scroll_height.current;
    previous_scroll_height.current = null;
  }, [history?.items.length]);

  const updateScrollState = useCallback((node: HTMLDivElement) => {
    const distance_from_bottom = Math.max(0, node.scrollHeight - node.scrollTop - node.clientHeight);
    is_pinned_to_bottom.current = distance_from_bottom <= PINNED_BOTTOM_THRESHOLD;
    setShowScrollBottom(distance_from_bottom > BOTTOM_THRESHOLD);
  }, []);

  const pinToBottomIfNeeded = useCallback(() => {
    const node = scroll_ref.current;
    if (!node || previous_scroll_height.current !== null || !is_pinned_to_bottom.current) {
      return;
    }
    node.scrollTop = node.scrollHeight;
    updateScrollState(node);
  }, [updateScrollState]);

  useLayoutEffect(() => {
    pinToBottomIfNeeded();
  }, [history?.items.length, live_run, pinToBottomIfNeeded]);

  useEffect(() => {
    const content = message_list_ref.current;
    if (!content || typeof ResizeObserver === "undefined") {
      return;
    }
    const observer = new ResizeObserver(() => pinToBottomIfNeeded());
    observer.observe(content);
    return () => observer.disconnect();
  }, [pinToBottomIfNeeded, session_id]);

  useLayoutEffect(() => {
    const node = scroll_ref.current;
    is_pinned_to_bottom.current = true;
    setShowScrollBottom(false);
    if (node) {
      const location = store.navigation.current_conversation_location;
      const matches_location = location?.session_id === session_id
        && location.child_task_id === child_task_id;
      if (matches_location && location.scroll_offset !== null) {
        node.scrollTop = location.scroll_offset;
        updateScrollState(node);
      } else {
        node.scrollTop = node.scrollHeight;
      }
    }
  }, [
    child_task_id,
    session_id,
    store.navigation,
    store.navigation.conversation_history_index,
    updateScrollState,
  ]);

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
      if (!quote_will_position_range) {
        is_pinned_to_bottom.current = false;
        target.scrollIntoView({ block: "center" });
        store.navigation.updateCurrentScrollOffset(node.scrollTop);
      }
      store.navigation.consumeConversationAnchor(message_id);
    }
  }, [
    history?.generation,
    history?.items,
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
    if (!is_quote_positioning_scroll && node.scrollTop < 96) {
      void loadPrevious();
    }
    store.navigation.updateCurrentScrollOffset(node.scrollTop);
    updateScrollState(node);
    if (is_quote_positioning_scroll) {
      return;
    }
    setSelectionAction(null);
    store.transient_focus.clear();
  }, [loadPrevious, store.navigation, updateScrollState]);

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
    const article = start.closest<HTMLElement>("[data-message-id]");
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
    const quote = selection_action.quote;
    setSelectionAction(null);
    window.getSelection()?.removeAllRanges();
    store.composer_quotes.add(session_id!, quote);
    store.transient_focus.clear();
    if (store.navigation.selected_child_task_id) store.closeChildTask();
  }, [selection_action, session_id, store]);

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
      : message.querySelector<HTMLElement>("[data-quote-root='true']"));
    const range = root ? quoteSourceRange(root, target) : null;
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

  const scrollToBottom = useCallback(() => {
    const node = scroll_ref.current;
    if (!node) {
      return;
    }
    is_pinned_to_bottom.current = true;
    setShowScrollBottom(false);
    node.scrollTo({ top: node.scrollHeight, behavior: "smooth" });
  }, []);

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
        input: { type: "unavailable" },
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

  useEffect(() => {
    setSummaryExpanded(false);
  }, [history?.generation, session_id]);

  const renderConversationRows = (rows: readonly ConversationRow[]) => (
    rows.map((row) => {
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
                  <MarkdownContent text={row.message.text} />
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
            message={row.message}
            on_attachment_click={setPreviewAttachment}
            on_quote_locate={(quote) => session_id
              ? store.locateTextQuoteSource(session_id, quote)
              : Promise.resolve(false)}
            on_source_open={(source_session) => {
              store.navigation.setListMode(source_session.lifecycle === "archived" ? "archived" : "active");
              void store.selectSession(source_session.session_id);
            }}
            source_session={all_sessions.find(
              (item) => item.session_id === sourceSessionId(row.message.source),
            )}
          />
        )
      : (
          <div className={styles.turn_with_tasks} key={row.key}>
            <AssistantTurn
              child_tasks={!child_task_id && row.run_id
                ? child_task_items.filter((item) => item.task.parent_run_id === row.run_id)
                : []}
              live_run={row.run_id === live_run?.run_id ? live_run : null}
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

  if (!session_id) {
    if (store.navigation.selected_draft_key) {
      return <EmptyConversation title="开始新会话" detail="输入消息或使用 / 指令。" />;
    }
    return <EmptyConversation title="选择一个会话" detail="从左侧工作空间中选择会话以查看消息。" />;
  }
  if (!history) {
    if (child_task_id) {
      return (
        <>
          <div className={styles.viewport}>
            <div className={styles.scroll}>
              <div className={styles.message_list}>
                {live_run
                  ? <LiveAssistantMessage onToolClick={openLiveToolDetail} run={live_run} />
                  : <EmptyConversation title="正在读取子任务" detail="正在从运行时获取可靠消息投影。" />}
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
    return <EmptyConversation title="正在读取会话" detail="正在从运行时获取可靠消息投影。" />;
  }
  if (history.items.length === 0 && !live_run) {
    if (!child_view) {
      return <EmptyConversation title="开始一段新会话" detail="从下方输入消息开始对话。" />;
    }
  }

  return (
    <div className={styles.viewport}>
      <div
        aria-label="消息列表"
        className={styles.scroll}
        onMouseUp={() => requestAnimationFrame(captureSelection)}
        onScroll={handleScroll}
        ref={scroll_ref}
      >
        <div className={styles.message_list} ref={message_list_ref}>
          {child_view && history.items.length === 0 && !live_run && (
            <EmptyConversation title={child_view.task.task.title} detail="子智能体尚未产生可展示的消息。" />
          )}
          {history.has_more && (
            <button className={styles.load_previous} disabled={history.is_loading_previous} onClick={() => void loadPrevious()} type="button">
              {history.is_loading_previous ? "正在加载更早消息…" : "加载更早消息"}
            </button>
          )}
          {history.load_error && <p className={styles.load_error}>{history.load_error}</p>}
          {renderConversationRows(conversation_rows)}
          {live_run && !conversation_rows.some((row) => row.type === "assistant_turn" && row.run_id === live_run.run_id)
            && (
              <LiveAssistantMessage
                child_tasks={!child_task_id
                  ? child_task_items.filter((item) => item.task.parent_run_id === live_run.run_id)
                  : []}
                on_child_open={!child_task_id && session_id
                  ? (item) => void store.openChildTask(session_id, item.task.child_task_id)
                  : undefined}
                onToolClick={openLiveToolDetail}
                run={live_run}
              />
            )}
        </div>
      </div>
      {selection_presence.mounted && retained_selection_action_ref.current && (
        <div
          aria-hidden={selection_presence.state === "exiting" ? true : undefined}
          className={styles.quote_bubble}
          data-presence={selection_presence.state}
          inert={selection_presence.state === "exiting" ? true : undefined}
          onTransitionEnd={selection_presence.onTransitionEnd}
          role="toolbar"
          style={{ left: retained_selection_action_ref.current.x, top: retained_selection_action_ref.current.y }}
        >
          <button onClick={submitSelectionQuote} type="button">
            <Icon name="quote" size={14} />引用
          </button>
        </div>
      )}
      {show_scroll_bottom && (
        <button
          aria-label="回到底部"
          className={styles.scroll_bottom}
          onClick={scrollToBottom}
          type="button"
        >
          <Icon name="arrow-down" size={16} />
        </button>
      )}
      <PresenceBoundary present={detail_state !== null}>
      {detail_state && (
        <ToolDetailDialog
          detail={detail_state.detail}
          error={detail_state.error}
          is_loading={detail_state.is_loading}
          on_close={() => setDetailState(null)}
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
  source: import("../../../generated/assistant-protocol").ConversationInputSourceSnapshot | undefined,
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

function sameConversationOwner(left: ConversationOwner, right: ConversationOwner): boolean {
  return left.type === right.type
    && left.session_id === right.session_id
    && (left.type !== "child_task"
      || (right.type === "child_task" && left.child_task_id === right.child_task_id));
}
