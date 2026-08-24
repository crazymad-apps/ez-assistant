import { observer } from "mobx-react-lite";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import type { SessionSummary, WorkspaceSummary } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { sessionTime, workspaceDisplayName } from "../sessionFormatters";
import styles from "./index.module.scss";

export const WorkspaceGroup = observer(function WorkspaceGroup(props: Readonly<{
  sessions: readonly SessionSummary[];
  workspace: WorkspaceSummary;
}>) {
  const store = useRootStore();
  const is_expanded = store.navigation.expanded_workspaces.has(props.workspace.workspace_id);
  const name = workspaceDisplayName(props.workspace.user_directory);
  const retained_session_count = [
    ...(store.projection.application?.active_sessions ?? []),
    ...(store.projection.application?.archived_sessions ?? []),
  ].filter((session) => session.workspace_id === props.workspace.workspace_id).length;

  async function removeWorkspace() {
    const session_note = retained_session_count > 0
      ? `\n\n已有 ${retained_session_count} 个会话会一并从侧栏隐藏，重新添加此目录后恢复显示。`
      : "";
    const confirmed = window.confirm(
      `移除工作空间“${name}”？\n\n不会删除本地目录或历史会话。${session_note}`,
    );
    if (confirmed) {
      await store.removeWorkspace(props.workspace.workspace_id);
    }
  }

  return (
    <section className={styles.workspace_group}>
      <div className={styles.workspace_row}>
        <button
          aria-expanded={is_expanded}
          className={styles.workspace_toggle}
          onClick={() => store.toggleWorkspace(props.workspace.workspace_id)}
          type="button"
        >
          <Icon name="folder" size={17} />
          <span>{name}</span>
        </button>
        {store.navigation.list_mode === "active" && (
          <DropdownMenu className={styles.workspace_menu_root}>
            <DropdownMenuTrigger
              aria-label={`${name} 工作空间操作`}
              className={styles.workspace_action}
            >
              <Icon name="more" size={15} />
            </DropdownMenuTrigger>
            <DropdownMenuContent aria-label={`${name} 工作空间操作`}>
              <DropdownMenuItem
                disabled={
                  store.connection.state !== "connected" ||
                  store.pending_session_action ||
                  store.pending_workspace_action
                }
                onSelect={() => void store.createSession(props.workspace.workspace_id)}
              >
                <Icon name="plus" size={15} />
                <span>在此新建会话</span>
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void store.openWorkspace(props.workspace.workspace_id)}>
                <Icon name="folder" size={15} />
                <span>打开工作目录</span>
              </DropdownMenuItem>
              <DropdownMenuItem onSelect={() => void store.copyWorkspacePath(props.workspace.user_directory)}>
                <Icon name="copy" size={15} />
                <span>复制目录路径</span>
              </DropdownMenuItem>
              <DropdownMenuItem
                className={styles.workspace_remove_action}
                disabled={
                  store.connection.state !== "connected" ||
                  store.pending_session_action ||
                  store.pending_workspace_action
                }
                onSelect={() => void removeWorkspace()}
              >
                <Icon name="trash" size={15} />
                <span>移除工作空间…</span>
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
      {is_expanded && (
        <SessionList id={`workspace-${props.workspace.workspace_id}`} sessions={props.sessions} />
      )}
    </section>
  );
});

export const SessionList = observer(function SessionList(props: Readonly<{
  id?: string;
  indent?: "nested" | "root";
  sessions: readonly SessionSummary[];
}>) {
  const store = useRootStore();

  return (
    <div className={styles.session_list} data-indent={props.indent ?? "nested"} id={props.id}>
      {props.sessions.map((session) => (
        <button
          aria-current={store.navigation.selected_session_id === session.session_id ? "page" : undefined}
          className={styles.session_row}
          key={session.session_id}
          onClick={() => void store.selectSession(session.session_id)}
          type="button"
        >
          <span className={styles.session_title}>{session.title}</span>
          {session.is_pinned && <Icon className={styles.pin_icon} name="pin" size={13} />}
          {session.active_run_id ? (
            <span className={styles.loading_ring} aria-label="正在运行" />
          ) : (
            <time className={styles.session_time}>{sessionTime(session)}</time>
          )}
        </button>
      ))}
    </div>
  );
});
