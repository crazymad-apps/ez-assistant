import { observer } from "mobx-react-lite";
import { useState } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { PresenceBoundary } from "../../../components/Presence";
import type { SessionSummary, WorkspaceSummary } from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { draftKeyForWorkspace } from "../../../stores/NewSessionDraftStore";
import { sessionTime } from "../sessionFormatters";
import { SessionActionDialog } from "../SessionActionDialog";
import styles from "./index.module.scss";

export const WorkspaceGroup = observer(function WorkspaceGroup(props: Readonly<{
  sessions: readonly SessionSummary[];
  workspace: WorkspaceSummary;
}>) {
  const store = useRootStore();
  const [remove_open, setRemoveOpen] = useState(false);
  const is_expanded = store.sidebar_navigation.expanded_workspaces.has(props.workspace.workspace_id);
  const duplicated = (store.sidebar_projection.application?.workspaces ?? []).filter((workspace) =>
    workspace.lifecycle === "active" && workspace.label === props.workspace.label
  ).length > 1;
  const name = `${props.workspace.label}${duplicated ? ` · ${props.workspace.workspace_id.slice(0, 8)}` : ""}`;
  const retained_session_count = [
    ...(store.sidebar_projection.application?.active_sessions ?? []),
    ...(store.sidebar_projection.application?.archived_sessions ?? []),
  ].filter((session) => session.workspace_id === props.workspace.workspace_id).length;

  async function removeWorkspace() {
    const removed = await store.removeWorkspace(props.workspace.workspace_id);
    if (removed) setRemoveOpen(false);
  }

  return (
    <section className={styles.workspace_group}>
      <div className={styles.workspace_row}>
        <button
          aria-expanded={is_expanded}
          className={styles.workspace_toggle}
          onClick={() => store.toggleWorkspace(props.workspace.workspace_id)}
          type="button"
          title={`${name} · ${props.workspace.user_directory ?? "仅使用私有目录"}${props.workspace.additional_directories.length ? ` · ${props.workspace.additional_directories.length} 个附加目录` : ""}`}
        >
          <Icon name="folder" size={17} />
          <span>{name}</span>
        </button>
        {store.sidebar_navigation.list_mode === "active" && (
          <DropdownMenu className={styles.workspace_menu_root}>
            <DropdownMenuTrigger
              aria-label={`${name} 项目操作`}
              className={styles.workspace_action}
            >
              <Icon name="more" size={15} />
            </DropdownMenuTrigger>
            <DropdownMenuContent aria-label={`${name} 项目操作`}>
              <DropdownMenuItem
                disabled={
                  store.sidebar_connection.state !== "connected" ||
                  store.pending_session_action ||
                  store.pending_workspace_action
                }
                onSelect={() => store.openNewSessionDraft(props.workspace.workspace_id, store.network.browsing_device)}
              >
                <Icon name="plus" size={15} />
                <span>在此新建会话</span>
                {(store.sidebar_context?.drafts ?? store.local_new_session_drafts).hasDraft(draftKeyForWorkspace(props.workspace.workspace_id)) && (
                  <em className={styles.draft_indicator}>有草稿</em>
                )}
              </DropdownMenuItem>
              {!store.network.browsing_device && store.local_files.native_host && <DropdownMenuItem onSelect={() => void store.openWorkspace(props.workspace.workspace_id)}>
                <Icon name="folder" size={15} />
                <span>打开工作目录</span>
              </DropdownMenuItem>}
              <DropdownMenuItem onSelect={() => void store.copyWorkspacePath(props.workspace.user_directory ?? props.workspace.agent_directory)}>
                <Icon name="copy" size={15} />
                <span>复制目录路径</span>
              </DropdownMenuItem>
              <DropdownMenuItem
                disabled={store.pending_workspace_action}
                onSelect={() => store.openWorkspaceEditor(props.workspace.workspace_id, store.network.browsing_device)}
              >
                <Icon name="edit" size={15} />
                <span>编辑项目…</span>
              </DropdownMenuItem>
              <DropdownMenuItem
                className={styles.workspace_remove_action}
                disabled={
                  store.sidebar_connection.state !== "connected" ||
                  store.pending_session_action ||
                  store.pending_workspace_action
                }
                onSelect={() => setRemoveOpen(true)}
              >
                <Icon name="trash" size={15} />
                <span>移除项目…</span>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
        <button
          aria-expanded={is_expanded}
          aria-label={`${is_expanded ? "收起" : "展开"} ${name}`}
          className={styles.workspace_chevron_button}
          onClick={() => store.toggleWorkspace(props.workspace.workspace_id)}
          type="button"
        >
          <Icon className={styles.workspace_chevron} name="chevron-down" size={14} />
        </button>
      </div>
      <Collapse open={is_expanded}>
        <SessionList id={`workspace-${props.workspace.workspace_id}`} sessions={props.sessions} />
      </Collapse>
      <PresenceBoundary present={remove_open}>
        {remove_open && (
          <SessionActionDialog
            confirm_label="移除项目"
            is_danger
            is_pending={store.pending_workspace_action}
            on_cancel={() => setRemoveOpen(false)}
            on_confirm={() => void removeWorkspace()}
            title={`移除项目“${name}”？`}
          >
            <p>不会删除本地目录或历史会话。</p>
            {retained_session_count > 0 && (
              <p>已有 {retained_session_count} 个会话会一并从侧栏隐藏，重新添加此目录后恢复显示。</p>
            )}
          </SessionActionDialog>
        )}
      </PresenceBoundary>
    </section>
  );
});

export const SessionList = observer(function SessionList(props: Readonly<{
  target_device?: string | null;
  id?: string;
  indent?: "nested" | "root";
  sessions: readonly SessionSummary[];
}>) {
  const store = useRootStore();
  const target = props.target_device === undefined ? store.network.browsing_device : props.target_device;

  return (
    <div className={styles.session_list} data-indent={props.indent ?? "nested"} id={props.id}>
      {props.sessions.map((session) => (
        <div
          className={styles.session_row}
          data-has-actions={store.sidebar_navigation.list_mode === "active"}
          data-selected={store.navigation.target_device === target && store.navigation.selected_session_id === session.session_id}
          key={session.session_id}
        >
          <button
            aria-current={store.navigation.target_device === target && store.navigation.selected_session_id === session.session_id ? "page" : undefined}
            className={styles.session_open}
            data-navigation-action
            onClick={() => void store.selectSession(session.session_id, target)}
            title={`${session.title} · ${session.workspace_id
              ? store.sidebar_projection.application?.workspaces.find((workspace) => workspace.workspace_id === session.workspace_id)?.label ?? "项目"
              : "无项目"}`}
            type="button"
          >
            <span className={`${styles.session_title} ${styles.title_refresh}`} key={session.title}>{session.title}</span>
          </button>
          {(store.local_files.desktop || (target === null && !store.remote_device)) && store.sidebar_navigation.list_mode === "active" && <button
            aria-label={`${store.isSessionPinned(session, target) ? "取消置顶" : "置顶"} ${session.title}`}
            className={styles.session_pin_action}
            disabled={store.sidebar_connection.state !== "connected" || store.pending_session_action || (store.local_files.desktop && (store.pins.busy || !store.pins.value.imported))}
            onClick={() => void store.setSessionPinned(session.session_id, !store.isSessionPinned(session, target), target)}
            title={store.isSessionPinned(session, target) ? "取消置顶" : "置顶"}
            type="button"
          ><Icon name={store.isSessionPinned(session, target) ? "pin" : "pin-outline"} size={14} /></button>}
          {session.active_run_id ? (
            <svg
              aria-label="正在运行"
              className={styles.loading_ring}
              role="img"
              viewBox="0 0 16 16"
            >
              <circle className={styles.loading_ring_track} cx="8" cy="8" r="6" />
              <circle className={styles.loading_ring_arc} cx="8" cy="8" r="6" />
            </svg>
          ) : <time className={styles.session_time}>{sessionTime(session)}</time>}
        </div>
      ))}
    </div>
  );
});
