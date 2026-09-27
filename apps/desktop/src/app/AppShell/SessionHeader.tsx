import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../components/DropdownMenu";
import { Icon } from "../../components/Icon";
import { InlineIconButton } from "../../components/InlineIconButton";
import { PresenceBoundary } from "../../components/Presence";
import { useInputMethodGuard } from "../../components/InputMethodGuard";
import type { PrepareDeleteSessionResult, SessionSummary } from "@ez-assistant/protocol";
import { SessionActionDialog } from "../../features/sessions/SessionActionDialog";
import { SessionNavigation } from "../../features/sessions/SessionNavigation";
import { isDraftCustomized } from "../../stores/NewSessionDraftStore";
import { useRootStore } from "../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const SessionHeader = observer(function SessionHeader({ session, sidebar_open, on_toggle_sidebar, on_search }: Readonly<{
  session?: SessionSummary;
  sidebar_open: boolean;
  on_toggle_sidebar: () => void;
  on_search: () => void;
}>) {
  const store = useRootStore();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(session?.title ?? "");
  const [delete_preview, setDeletePreview] = useState<PrepareDeleteSessionResult | null>(null);
  const [clear_open, setClearOpen] = useState(false);
  const [proxy_error, setProxyError] = useState<string | null>(null);
  const cancel_blur_ref = useRef(false);
  const input_method = useInputMethodGuard();
  const is_archived = session?.lifecycle === "archived";
  const new_session_draft = store.new_session_drafts.get(store.navigation.selected_draft_key);
  const session_view = session ? store.projection.session_views.get(session.session_id) : undefined;
  const is_controller = session?.role === "controller";
  const is_proxied = Boolean(session?.proxy);
  const controller_available = store.projection.application?.controller_availability.status === "available";
  const blocks_archive = Boolean(
    session?.active_run_id
    || session?.queued_input_count
    || session?.pending_approval_count,
  );
  const blocks_clear = Boolean(
    is_archived
    || session?.active_run_id
    || session?.queued_input_count
    || session?.pending_approval_count
    || session?.active_child_count
    || session?.active_compaction,
  );

  useEffect(() => {
    setEditing(false);
    setTitle(session?.title ?? "");
    setClearOpen(false);
    setProxyError(null);
  }, [session?.session_id, session?.title, store.navigation.selected_draft_key]);

  function finishEdit() {
    if (!editing || !session) {
      return;
    }
    setEditing(false);
    const next = title.trim();
    if (!next || next === session.title) {
      setTitle(session.title);
      return;
    }
    void store.renameSession(session.session_id, next).then((saved) => {
      if (!saved) {
        setTitle(session.title);
      }
    });
  }

  async function prepareDelete() {
    if (!session) {
      return;
    }
    const preview = await store.prepareDeleteSession(session.session_id);
    if (preview) {
      setDeletePreview(preview);
    }
  }

  async function confirmDelete() {
    if (!delete_preview) {
      return;
    }
    const deleted = await store.deleteSession(delete_preview);
    if (deleted) {
      setDeletePreview(null);
    }
  }

  async function confirmClear() {
    if (!session || !session_view) {
      return;
    }
    const result = await store.clearSession(session.session_id, session_view.conversation_generation);
    if (result) {
      setClearOpen(false);
    }
  }

  async function toggleProxy() {
    if (!session || is_controller) {
      return;
    }
    setProxyError(null);
    const saved = await store.setSessionProxy(session.session_id, !is_proxied);
    if (!saved) {
      setProxyError(store.interaction_error ?? "未能更新主控代理状态。");
    }
  }

  return (
    <header aria-label="会话标题栏" className={styles.session_header} data-sidebar-open={sidebar_open} data-tauri-drag-region="deep">
      {!sidebar_open && <SessionNavigation on_search={on_search} on_toggle={on_toggle_sidebar} sidebar_open={false} />}
      <div className={styles.session_title_slot}>
        {new_session_draft ? null : editing && session ? (
          <input placeholder="请输入"
            aria-label="会话标题"
            autoFocus
            className={styles.session_title_input}
            maxLength={80}
            onBlur={() => {
              if (cancel_blur_ref.current) {
                cancel_blur_ref.current = false;
                return;
              }
              finishEdit();
            }}
            onChange={(event) => setTitle(event.currentTarget.value)}
            onCompositionEnd={input_method.onCompositionEnd}
            onCompositionStart={input_method.onCompositionStart}
            onKeyDown={(event) => {
              if (input_method.shouldIgnoreKeyDown(event)) {
                return;
              }
              if (event.key === "Enter") {
                event.preventDefault();
                finishEdit();
              } else if (event.key === "Escape") {
                event.preventDefault();
                cancel_blur_ref.current = true;
                setEditing(false);
                setTitle(session.title);
              }
            }}
            onKeyUp={input_method.onKeyUp}
            value={title}
          />
        ) : (
          <button
            className={`${styles.session_title_button} ${styles.title_refresh}`}
            disabled={!session || is_archived}
            onClick={() => setEditing(true)}
            title={session?.title}
            type="button"
            key={session?.title ?? "new-session"}
          >
            {session?.title ?? "新会话"}
          </button>
        )}
        {session && is_controller && (
          <span className={styles.session_role_badge}>主控</span>
        )}
        {session?.custom_agent && <span className={styles.session_role_badge} title="创建会话时冻结的智能体配置">
          {session.custom_agent.name} · {session.custom_agent.source.type === "center" ? "企业" : "个人"}
        </span>}
        {session && !is_controller && (
          <div
            className={styles.proxy_control}
            title={!controller_available ? "主控会话暂不可用" : undefined}
          >
            <span>主控代理</span>
            <button
              aria-checked={is_proxied}
              aria-label="主控代理"
              disabled={
                is_archived
                || !controller_available
                || store.pending_proxy_session_id === session.session_id
              }
              onClick={() => void toggleProxy()}
              role="switch"
              type="button"
            >
              <i />
            </button>
          </div>
        )}
        {is_proxied && session && session.queued_input_count > 0 && (
          <span className={styles.proxy_queue_hint}>现有队列处理完毕后报告最新结果</span>
        )}
      </div>
      <div className={styles.session_actions}>
        <DropdownMenu>
          <DropdownMenuTrigger aria-label="更多会话操作" className={styles.header_icon_button} inlineIcon="more" />
          <DropdownMenuContent aria-label="会话操作" className={styles.session_menu}>
            <DropdownMenuItem onSelect={() => store.resetSidebarLayout()}>
              <span>恢复默认布局</span>
            </DropdownMenuItem>
            {new_session_draft && (
              <DropdownMenuItem
                onSelect={() => {
                  if (isDraftCustomized(
                    new_session_draft,
                    store.new_session_drafts.default_approval_mode,
                    store.new_session_drafts.default_model_selection,
                  )) {
                    setClearOpen(true);
                  } else {
                    void store.clearNewSessionDraft(new_session_draft.key);
                  }
                }}
              >
                <span>清空当前草稿…</span>
              </DropdownMenuItem>
            )}
            {session && (
              <DropdownMenuItem
                disabled={store.pending_session_action}
                onSelect={() => void store.exportSession(session.session_id, session.title)}
              >
                <span>导出 Markdown</span>
              </DropdownMenuItem>
            )}
            {!is_archived && session && (
              <>
                <div aria-orientation="horizontal" className={styles.session_menu_separator} role="separator" />
                <DropdownMenuItem
                  disabled={blocks_clear || !session_view || store.pending_session_action || store.composer_pending}
                  onSelect={() => setClearOpen(true)}
                  title={blocks_clear ? "请等待运行、审批、队列、子任务或压缩结束" : undefined}
                >
                  <span>清空会话历史…</span>
                </DropdownMenuItem>
              </>
            )}
            {!is_archived && session && !is_controller && (
              <>
                <DropdownMenuItem disabled={store.pending_session_action} onSelect={() => void store.setSessionPinned(session.session_id, !session.is_pinned)}>
                  <span>{session.is_pinned ? "取消置顶" : "置顶"}</span>
                  {session.is_pinned && <Icon name="check" size={14} />}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={blocks_archive || store.pending_session_action}
                  onSelect={() => void store.archiveSession(session.session_id)}
                  title={blocks_archive ? "运行、队列或审批尚未结束" : undefined}
                >
                  <span>归档会话</span>
                </DropdownMenuItem>
              </>
            )}
            {is_archived && session && (
              <DropdownMenuItem onSelect={() => void store.restoreSession(session.session_id)}>
                <span>恢复会话</span>
              </DropdownMenuItem>
            )}
            {session && !is_controller && (
              <DropdownMenuItem
                className={styles.delete_action}
                disabled={blocks_archive || store.pending_session_action}
                onSelect={() => void prepareDelete()}
                title={blocks_archive ? "运行、队列或审批尚未结束" : undefined}
              >
                <span>永久删除</span>
              </DropdownMenuItem>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
        <InlineIconButton
          label={store.navigation.effective_right_sidebar_open ? "收起资源栏" : "展开资源栏"}
          className={styles.context_toggle}
          onClick={() => store.toggleRightSidebar()}
          icon="sidebar-right"
          size={17}
        />
      </div>
      <PresenceBoundary present={delete_preview !== null}>
      {delete_preview && (
        <SessionActionDialog
          confirm_label="永久删除"
          is_danger
          is_pending={store.pending_session_action}
          on_cancel={() => setDeletePreview(null)}
          on_confirm={() => void confirmDelete()}
          title="永久删除这个会话？"
        >
          <p><strong>{delete_preview.session.title}</strong> 将从本机永久移除，此操作无法撤销。</p>
          <p>
            将删除 {delete_preview.impact.message_count} 条消息、{delete_preview.impact.run_count} 条运行记录、
            {delete_preview.impact.child_task_count} 个子任务和 {delete_preview.impact.attachment_count} 个附件引用。
            工作目录中的用户文件不会被删除。
          </p>
          {store.resource_workspace.runningTerminalCount(`session:${delete_preview.session.session_id}`) > 0 && (
            <p>同时关闭该会话的 {store.resource_workspace.runningTerminalCount(`session:${delete_preview.session.session_id}`)} 个终端，终端内运行的进程将结束。</p>
          )}
          {store.interaction_error && <p role="alert">{store.interaction_error}</p>}
        </SessionActionDialog>
      )}
      </PresenceBoundary>
      <PresenceBoundary present={clear_open && Boolean(session)}>
      {clear_open && session && (
        <SessionActionDialog
          confirm_label="清空历史"
          is_danger
          is_pending={store.pending_session_action}
          on_cancel={() => setClearOpen(false)}
          on_confirm={() => void confirmClear()}
          pending_label="正在清空…"
          title="清空这个会话的历史？"
        >
          <p>将永久删除 <strong>{session.title}</strong> 的消息、运行、队列、审批和工作状态，并重建系统上下文。</p>
          <p>会话身份、工作目录、附件文件和私有目录会保留，此操作无法撤销。</p>
          <p>{is_controller ? "该会话仍保留主控身份。" : is_proxied ? "清空后当前主控代理关系仍保留。" : "该会话仍保持普通会话身份。"}</p>
          {store.interaction_error && <p role="alert">{store.interaction_error}</p>}
        </SessionActionDialog>
      )}
      </PresenceBoundary>
      <PresenceBoundary present={clear_open && Boolean(new_session_draft)}>
      {clear_open && new_session_draft && (
        <SessionActionDialog
          confirm_label="清空草稿"
          is_pending={false}
          on_cancel={() => setClearOpen(false)}
          on_confirm={() => {
            setClearOpen(false);
            void store.clearNewSessionDraft(new_session_draft.key);
          }}
          title="清空当前草稿？"
        >
          <p>将移除尚未发送的文本、附件和发送设置，不会创建或删除任何会话。</p>
        </SessionActionDialog>
      )}
      </PresenceBoundary>
      {proxy_error && <div className={styles.session_header_error} role="alert">{proxy_error}</div>}
    </header>
  );
});
