import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../components/DropdownMenu";
import { Icon } from "../../components/Icon";
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
  const cancel_blur_ref = useRef(false);
  const recent = [...(store.projection.application?.active_sessions ?? [])]
    .sort((left, right) => (right.updated_at_ms ?? 0) - (left.updated_at_ms ?? 0))
    .slice(0, 8);
  const is_archived = session?.lifecycle === "archived";
  const blocks_archive = Boolean(
    session?.active_run_id
    || session?.queued_input_count
    || session?.pending_approval_count,
  );
  const status = session ? sessionStatus(session) : null;

  useEffect(() => {
    setEditing(false);
    setTitle(session?.title ?? "");
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
            onKeyDown={(event) => {
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
              <>
                <DropdownMenuItem onSelect={() => void store.setSessionPinned(session.session_id, !session.is_pinned)}>
                  <span>固定会话</span>
                  {session.is_pinned && <Icon name="check" size={14} />}
                </DropdownMenuItem>
                <DropdownMenuItem
                  disabled={blocks_archive}
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
            {session && (
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
