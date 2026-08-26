import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../components/DropdownMenu";
import { Icon } from "../../components/Icon";
import { useInputMethodGuard } from "../../components/InputMethodGuard";
import type { PrepareDeleteSessionResult, SessionSummary } from "../../generated/assistant-protocol";
import { SessionActionDialog } from "../../features/sessions/SessionActionDialog";
import { useRootStore } from "../../stores/RootStoreContext";
import styles from "./index.module.scss";

export const SessionHeader = observer(function SessionHeader({ session }: Readonly<{
  session?: SessionSummary;
}>) {
  const store = useRootStore();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(session?.title ?? "");
  const [delete_preview, setDeletePreview] = useState<PrepareDeleteSessionResult | null>(null);
  const [clear_open, setClearOpen] = useState(false);
  const [proxy_error, setProxyError] = useState<string | null>(null);
  const cancel_blur_ref = useRef(false);
  const input_method = useInputMethodGuard();
  const recent = [...(store.projection.application?.active_sessions ?? [])]
    .sort((left, right) => (right.updated_at_ms ?? 0) - (left.updated_at_ms ?? 0))
    .slice(0, 8);
  const is_archived = session?.lifecycle === "archived";
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
  const status = session ? sessionStatus(session) : null;

  useEffect(() => {
    setEditing(false);
    setTitle(session?.title ?? "");
    setClearOpen(false);
    setProxyError(null);
  }, [session?.session_id, session?.title]);

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
    <header aria-label="会话标题栏" className={styles.session_header}>
      <div className={styles.session_navigation}>
        <button
          aria-label="返回上一处会话位置"
          disabled={!store.navigation.can_go_back}
          onClick={() => void store.navigateBack()}
          type="button"
        >
          <Icon name="chevron-left" size={15} />
        </button>
        <button
          aria-label="前往下一处会话位置"
          disabled={!store.navigation.can_go_forward}
          onClick={() => void store.navigateForward()}
          type="button"
        >
          <Icon name="chevron-right" size={15} />
        </button>
        <DropdownMenu>
          <DropdownMenuTrigger aria-label="选择最近会话" className={styles.session_switcher}>
            <Icon name="chevron-down" size={16} />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" aria-label="最近会话" className={styles.recent_menu}>
            {recent.map((item) => (
              <DropdownMenuItem
                aria-current={item.session_id === session?.session_id ? "page" : undefined}
                key={item.session_id}
                onSelect={() => void store.selectSession(item.session_id)}
              >
                <span>{item.title}</span>
                {item.session_id === session?.session_id && <Icon name="check" size={14} />}
              </DropdownMenuItem>
            ))}
            {recent.length === 0 && <span className={styles.recent_empty}>暂无会话</span>}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <div className={styles.session_title_slot}>
        {editing && session ? (
          <input
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
            className={styles.session_title_button}
            disabled={!session || is_archived}
            onClick={() => setEditing(true)}
            title={session?.title}
            type="button"
          >
            {session?.title ?? "新会话"}
          </button>
        )}
        {status && (
          <span className={styles.session_status} data-tone={status.tone}>
            <i aria-hidden="true" />
            {status.label}
          </span>
        )}
        {session && is_controller && (
          <span className={styles.session_role_badge}>主控</span>
        )}
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
          <DropdownMenuTrigger aria-label="更多会话操作">
            <Icon name="more" size={17} />
          </DropdownMenuTrigger>
          <DropdownMenuContent aria-label="会话操作" className={styles.session_menu}>
            {session && (
              <DropdownMenuItem
                disabled={store.pending_session_action}
                onSelect={() => void store.exportSession(session.session_id, session.title)}
              >
                <span>导出 Markdown</span>
              </DropdownMenuItem>
            )}
            {!is_archived && session && (
              <DropdownMenuItem
                className={styles.clear_action}
                disabled={blocks_clear || !session_view || store.pending_session_action || store.composer_pending}
                onSelect={() => setClearOpen(true)}
                title={blocks_clear ? "请等待运行、审批、队列、子任务或压缩结束" : undefined}
              >
                <span>清空会话历史…</span>
              </DropdownMenuItem>
            )}
            {!is_archived && session && (
              <>
                <DropdownMenuItem onSelect={() => void store.setSessionPinned(session.session_id, !session.is_pinned)}>
                  <span>固定会话</span>
                  {session.is_pinned && <Icon name="check" size={14} />}
                </DropdownMenuItem>
                {!is_controller && <DropdownMenuItem
                  disabled={blocks_archive}
                  onSelect={() => void store.archiveSession(session.session_id)}
                  title={blocks_archive ? "运行、队列或审批尚未结束" : undefined}
                >
                  <span>归档会话</span>
                </DropdownMenuItem>}
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
        {!store.navigation.right_sidebar_open && (
          <button
            aria-label="展开当前上下文"
            className={styles.context_toggle}
            onClick={() => store.toggleRightSidebar()}
            type="button"
          >
            <Icon name="sidebar-right" size={17} />
          </button>
        )}
      </div>
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
        </SessionActionDialog>
      )}
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
      {proxy_error && <div className={styles.session_header_error} role="alert">{proxy_error}</div>}
    </header>
  );
});

function sessionStatus(session: SessionSummary): Readonly<{
  label: string;
  tone: "neutral" | "success" | "warning" | "active";
}> {
  if (session.lifecycle === "archived") {
    return { label: "已归档", tone: "neutral" };
  }
  if (session.active_compaction) {
    return { label: "压缩中", tone: "active" };
  }
  if (session.pending_approval_count > 0) {
    return { label: "等待审批", tone: "warning" };
  }
  if (session.active_run_id) {
    return { label: "运行中", tone: "active" };
  }
  if (session.queued_input_count > 0) {
    return { label: "排队", tone: "active" };
  }
  return { label: "空闲", tone: "success" };
}
