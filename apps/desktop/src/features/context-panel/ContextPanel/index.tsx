import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import type { AttachmentSummary, SystemContextSnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { AttachmentPreviewDialog } from "../AttachmentPreviewDialog";
import { SystemContextDialog } from "../SystemContextDialog";
import { ContextRing, ContextSection } from "./ContextSection";
import {
  childStatusLabel,
  formatApprovalMode,
  formatBytes,
  formatModelIdentity,
  formatNullableTokens,
  formatRunTime,
  formatTokens,
  formatVariant,
  runStatusLabel,
  sessionStatusLabel,
} from "./contextDisplay";
import styles from "./index.module.scss";

type ContextSectionKey = "session" | "workspace" | "attachments" | "children" | "runs";

export const ContextPanel = observer(function ContextPanel() {
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = application?.active_sessions.find((item) => item.session_id === session_id)
    ?? application?.archived_sessions.find((item) => item.session_id === session_id);
  const workspace = application?.workspaces.find((item) => item.workspace_id === session?.workspace_id);
  const session_view = session_id ? store.projection.session_views.get(session_id) : undefined;
  const attachments = session_view?.attachments ?? [];
  const [preview_attachment, setPreviewAttachment] = useState<AttachmentSummary | null>(null);
  const [system_context, setSystemContext] = useState<SystemContextSnapshot | null>(null);
  const [system_context_error, setSystemContextError] = useState<string | null>(null);
  const [system_context_loading, setSystemContextLoading] = useState(false);
  const [locating_run_id, setLocatingRunId] = useState<string | null>(null);
  const [section_state, setSectionState] = useState<Record<string, Partial<Record<ContextSectionKey, boolean>>>>({});
  const model = application?.models.find((item) => item.model_key === session?.model_key);
  const section_owner = session_id ?? "unselected";
  const sectionIsOpen = (section: ContextSectionKey) => section_state[section_owner]?.[section] ?? true;
  const toggleSection = (section: ContextSectionKey) => {
    setSectionState((current) => ({
      ...current,
      [section_owner]: {
        ...current[section_owner],
        [section]: !(current[section_owner]?.[section] ?? true),
      },
    }));
  };

  useEffect(() => {
    setSystemContext(null);
    setSystemContextError(null);
    setSystemContextLoading(false);
  }, [session_id]);

  const locateRun = async (run_id: string) => {
    if (!session_id || locating_run_id) {
      return;
    }
    setLocatingRunId(run_id);
    store.navigation.closeChildTask();
    await store.locateConversationRun(session_id, run_id);
    setLocatingRunId(null);
  };

  const openSystemContext = async () => {
    if (!session_id || system_context_loading) return;
    setSystemContextLoading(true);
    setSystemContextError(null);
    try {
      setSystemContext(await store.getSystemContext(session_id));
    } catch (error: unknown) {
      setSystemContextError(error instanceof Error ? error.message : "无法读取当前会话的 System Context。");
    } finally {
      setSystemContextLoading(false);
    }
  };

  return (
    <aside className={styles.panel} aria-label="当前上下文">
      <header className={styles.panel_header}>
        <h2>当前上下文</h2>
        <button aria-label="收起当前上下文" onClick={() => store.toggleRightSidebar()} type="button">
          <Icon name="sidebar-right" size={17} />
        </button>
      </header>
      <div className={styles.panel_scroll}>
        <ContextSection is_open={sectionIsOpen("session")} on_toggle={() => toggleSection("session")} title="会话">
          {session ? (
            <dl className={styles.definition_list}>
              <div><dt>状态</dt><dd>{sessionStatusLabel(
                session.lifecycle,
                session_view?.active_run?.status,
                session_view?.approvals.items.length ?? 0,
                session.resume_required,
              )}</dd></div>
              <div><dt>模型</dt><dd>{formatModelIdentity(model?.display_name, session.model_key)}</dd></div>
              <div><dt>执行方式</dt><dd>{formatVariant(session.current_variant)} · {formatApprovalMode(session.approval_mode)}</dd></div>
              <div><dt>消息</dt><dd>{session.message_count}</dd></div>
            </dl>
          ) : <p className={styles.empty_row}>尚未选择会话</p>}
          {session_view?.usage.context && (
            <div className={styles.context_usage}>
              <ContextRing basis_points={session_view.usage.context.usage_basis_points} />
              <div>
                <strong>上下文窗口</strong>
                <span>{formatTokens(session_view.usage.context.used_tokens)} / {formatTokens(session_view.usage.context.window_tokens)}</span>
              </div>
              <b>{(session_view.usage.context.usage_basis_points / 100).toFixed(1)}%</b>
            </div>
          )}
          {session_view?.usage.previous_turn && (
            <dl className={`${styles.definition_list} ${styles.usage_list}`}>
              <div><dt>上一轮输入</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.input_tokens)}</dd></div>
              <div><dt>上一轮输出</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.output_tokens)}</dd></div>
              <div><dt>缓存命中</dt><dd>{formatNullableTokens(session_view.usage.previous_turn.cached_input_tokens)}</dd></div>
            </dl>
          )}
          {session_view?.usage.accumulated && (
            <dl className={`${styles.definition_list} ${styles.usage_list}`}>
              <div><dt>会话 Token</dt><dd>{formatNullableTokens(session_view.usage.accumulated.total_tokens)}</dd></div>
            </dl>
          )}
          {session && (
            <button className={styles.system_context_row} onClick={() => void openSystemContext()} type="button">
              <strong>System Context</strong>
              <em>{system_context_loading ? "读取中…" : "查看原文"}</em>
            </button>
          )}
          {system_context_error && <p className={styles.context_error}>{system_context_error}</p>}
        </ContextSection>
        <ContextSection is_open={sectionIsOpen("workspace")} on_toggle={() => toggleSection("workspace")} title="Workspace">
          {workspace && session ? (
            <>
              <div className={styles.path_row} title={workspace.user_directory}>
                <Icon name="folder" size={16} />
                <span>{workspace.user_directory}</span>
              </div>
              <div className={styles.workspace_actions}>
                <button
                  onClick={() => void store.openWorkspace(workspace.workspace_id)}
                  title="在文件管理器中打开"
                  type="button"
                >
                  <Icon name="folder" size={14} />
                  打开目录
                </button>
                <button
                  onClick={() => void store.copyWorkspacePath(workspace.user_directory)}
                  title="复制工作目录路径"
                  type="button"
                >
                  <Icon name="copy" size={14} />
                  复制路径
                </button>
              </div>
              <p className={styles.workspace_note}>Shell 仍以当前用户权限运行，Workspace 不是强沙盒。</p>
            </>
          ) : <p className={styles.empty_row}>未绑定目录</p>}
        </ContextSection>
        <ContextSection
          is_open={sectionIsOpen("attachments")}
          on_toggle={() => toggleSection("attachments")}
          title={`会话附件 · ${attachments.length}`}
        >
          {attachments.length > 0 ? (
            <div className={styles.attachment_rows}>
              {attachments.map((attachment) => (
                <button
                  disabled={attachment.state !== "ready"}
                  key={attachment.attachment_id}
                  onClick={() => setPreviewAttachment(attachment)}
                  title={attachment.original_name}
                  type="button"
                >
                  <Icon name="paperclip" size={14} />
                  <span>{attachment.original_name}</span>
                  <small>{attachment.state === "ready" ? formatBytes(attachment.size_bytes) : "不可用"}</small>
                </button>
              ))}
            </div>
          ) : <p className={styles.empty_row}>暂无会话附件</p>}
        </ContextSection>
        {session_id && session_view && session_view.child_tasks.length > 0 && (
          <ContextSection
            is_open={sectionIsOpen("children")}
            on_toggle={() => toggleSection("children")}
            title={`子任务 · ${session_view.child_tasks.length}`}
          >
            <div className={styles.child_task_rows}>
              {session_view.child_tasks.map((item) => (
                <button
                  data-selected={store.navigation.selected_child_task_id === item.task.child_task_id}
                  key={item.task.child_task_id}
                  onClick={() => void store.openChildTask(session_id, item.task.child_task_id)}
                  title={item.task.title}
                  type="button"
                >
                  <i data-status={item.task.status} />
                  <span>{item.task.title}</span>
                  <small>{item.pending_approval_count > 0 ? "待审批" : childStatusLabel(item.task.status)}</small>
                </button>
              ))}
            </div>
          </ContextSection>
        )}
        <ContextSection
          is_open={sectionIsOpen("runs")}
          on_toggle={() => toggleSection("runs")}
          title={`运行记录 · ${session_view?.runs.length ?? 0}`}
        >
          {session_view && session_view.runs.length > 0 ? (
            <div className={styles.run_rows}>
              {[...session_view.runs].reverse().map((run) => (
                <button
                  disabled={locating_run_id !== null}
                  key={run.run_id}
                  onClick={() => void locateRun(run.run_id)}
                  type="button"
                >
                  <i data-status={run.status} />
                  <span>
                    <strong>运行 #{run.attempt} · {formatVariant(run.variant)}</strong>
                    <small>{formatRunTime(run.finished_at_ms ?? run.created_at_ms)} · {run.tools?.length ?? 0} 个工具</small>
                  </span>
                  <em>{locating_run_id === run.run_id ? "定位中…" : runStatusLabel(run.status)}</em>
                </button>
              ))}
            </div>
          ) : <p className={styles.empty_row}>还没有运行记录</p>}
        </ContextSection>
      </div>
      {preview_attachment && (
        <AttachmentPreviewDialog
          attachment={preview_attachment}
          on_close={() => setPreviewAttachment(null)}
        />
      )}
      {system_context && (
        <SystemContextDialog on_close={() => setSystemContext(null)} snapshot={system_context} />
      )}
    </aside>
  );
});
