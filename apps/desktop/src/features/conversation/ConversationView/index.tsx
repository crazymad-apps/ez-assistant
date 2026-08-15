import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type {
  AttachmentSummary,
  ConversationOwner,
  MessageId,
  ToolCallId,
} from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import type { LiveToolSnapshot } from "../../../stores/LiveExecutionStore";
import { useRootStore } from "../../../stores/RootStoreContext";
import { ToolDetailDialog, type ToolDetailView } from "../ToolDetailDialog";
import { SessionActionDialog } from "../../sessions/SessionActionDialog";
import { AttachmentPreviewDialog } from "../../context-panel/AttachmentPreviewDialog";
import { mergeChildTaskItems } from "../childTaskPresentation";
import { ChildTaskTree } from "./ChildTaskTree";
import { groupConversationTurns } from "./conversationRows";
import { AssistantTurn, EmptyConversation, LiveAssistantMessage, UserMessage } from "./MessageViews";
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
  const is_pinned_to_bottom = useRef(true);
  const [show_scroll_bottom, setShowScrollBottom] = useState(false);
  const [detail_state, setDetailState] = useState<{
    detail: ToolDetailView | null;
    error: string | null;
    is_loading: boolean;
  } | null>(null);
  const [fork_point, setForkPoint] = useState<MessageId | null>(null);
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const conversation_rows = useMemo(
    () => groupConversationTurns(history?.items ?? [], child_task_id !== null),
    [child_task_id, history?.items],
  );

  const owner: ConversationOwner | null = history?.owner ?? (session_id
    ? { type: "main_session", session_id }
    : null);
  const session = store.projection.application?.active_sessions.find((item) => item.session_id === session_id)
    ?? store.projection.application?.archived_sessions.find((item) => item.session_id === session_id);
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const child_task_items = session_id
    ? mergeChildTaskItems(
        session_view?.child_tasks ?? [],
        store.live_execution.childTasksForSession(session_id),
        (task_id) => store.live_execution.runForChildTask(task_id),
      )
    : [];
  const represented_runs = new Set(conversation_rows.flatMap((row) => (
    row.type === "assistant_turn" && row.run_id ? [row.run_id] : []
  )));
  if (live_run) {
    represented_runs.add(live_run.run_id);
  }
  const unmatched_child_tasks = child_task_id
    ? []
    : child_task_items.filter((item) => !represented_runs.has(item.task.parent_run_id));
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
      node.scrollTop = node.scrollHeight;
    }
  }, [child_task_id, session_id]);

  useLayoutEffect(() => {
    const message_id = store.navigation.conversation_anchor_message_id;
    const node = scroll_ref.current;
    if (!message_id || !node) {
      return;
    }
    const target = node.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(message_id)}"]`);
    if (target) {
      is_pinned_to_bottom.current = false;
      target.scrollIntoView({ block: "center" });
      store.navigation.consumeConversationAnchor(message_id);
    }
  }, [history?.items, store.navigation, store.navigation.conversation_anchor_message_id]);

  const handleScroll = useCallback(() => {
    const node = scroll_ref.current;
    if (!node) {
      return;
    }
    if (node.scrollTop < 96) {
      void loadPrevious();
    }
    updateScrollState(node);
  }, [loadPrevious, updateScrollState]);

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
        status: tool.status,
        input: { type: "unavailable" },
        result_summary: null,
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

  if (!session_id) {
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
                  : <EmptyConversation title="正在读取子任务" detail="正在从 Runtime 获取可靠消息投影。" />}
              </div>
            </div>
          </div>
          {detail_state && (
            <ToolDetailDialog
              detail={detail_state.detail}
              error={detail_state.error}
              is_loading={detail_state.is_loading}
              on_close={() => setDetailState(null)}
            />
          )}
        </>
      );
    }
    return <EmptyConversation title="正在读取会话" detail="正在从 Runtime 获取可靠消息投影。" />;
  }
  if (history.items.length === 0 && !live_run) {
    if (!child_view) {
      return <EmptyConversation title="开始一段新会话" detail="从下方输入消息开始对话。" />;
    }
  }

  return (
    <div className={styles.viewport}>
      <div aria-label="消息列表" className={styles.scroll} onScroll={handleScroll} ref={scroll_ref}>
        <div className={styles.message_list} ref={message_list_ref}>
          {child_view && history.items.length === 0 && !live_run && (
            <EmptyConversation title={child_view.task.task.title} detail="子 Agent 尚未产生可展示的消息。" />
          )}
          {history.has_more && (
            <button className={styles.load_previous} disabled={history.is_loading_previous} onClick={() => void loadPrevious()} type="button">
              {history.is_loading_previous ? "正在加载更早消息…" : "加载更早消息"}
            </button>
          )}
          {history.load_error && <p className={styles.load_error}>{history.load_error}</p>}
          {conversation_rows.map((row) => row.type === "user"
            ? (
                <UserMessage
                  attachments={(row.message.attachment_ids ?? []).flatMap((id) => {
                    const attachment = attachment_by_id.get(id);
                    return attachment ? [attachment] : [];
                  })}
                  key={row.message.message_id}
                  message={row.message}
                  on_attachment_click={setPreviewAttachment}
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
                    onLiveToolClick={openLiveToolDetail}
                    onToolClick={openToolDetail}
                  />
                </div>
              ))}
          {!child_task_id && session_id && (
            <ChildTaskTree
              items={unmatched_child_tasks}
              on_open={(item) => void store.openChildTask(session_id, item.task.child_task_id)}
            />
          )}
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
      {detail_state && (
        <ToolDetailDialog
          detail={detail_state.detail}
          error={detail_state.error}
          is_loading={detail_state.is_loading}
          on_close={() => setDetailState(null)}
        />
      )}
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
      {preview_attachment && (
        <AttachmentPreviewDialog
          attachment={preview_attachment}
          on_close={() => setPreviewAttachment(null)}
        />
      )}
    </div>
  );
});
