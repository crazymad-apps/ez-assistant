import { Button } from "../../../components/Button";
import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { Tooltip } from "../../../components/Tooltip";
import { useRootStore } from "../../../stores/RootStoreContext";
import { draftKeyForWorkspace } from "../../../stores/NewSessionDraftStore";
import { SessionList, WorkspaceGroup } from "./WorkspaceGroup";
import { AccountMenu } from "../../runtime-access/AccountMenu";
import { useApplicationConnection } from "../../runtime-access/ApplicationConnectionContext";
import { SessionNavigation } from "../SessionNavigation";
import { RemovedWorkspacesDialog } from "../../workspaces/RemovedWorkspacesDialog";
import styles from "./index.module.scss";

export const SessionSidebar = observer(function SessionSidebar({ hidden = false, on_search, on_toggle }: Readonly<{
  hidden?: boolean;
  on_search: () => void;
  on_toggle: () => void;
}>) {
  const store = useRootStore();
  const application_connection = useApplicationConnection();
  const application = store.projection.application;
  const [pinned_section_open, setPinnedSectionOpen] = useState(true);
  const [workspace_section_open, setWorkspaceSectionOpen] = useState(true);
  const [restore_dialog_open, setRestoreDialogOpen] = useState(false);
  const [unbound_section_open, setUnboundSectionOpen] = useState(true);
  const previous_pinned_ids = useRef<Set<string> | null>(null);
  const [list_loading, setListLoading] = useState(false);
  const [list_error, setListError] = useState<string | null>(null);
  const sessions =
    store.navigation.list_mode === "active"
      ? (application?.active_sessions ?? [])
      : (application?.archived_sessions ?? []);
  const workspaces = application?.workspaces ?? [];
  const active_workspaces = workspaces.filter((workspace) => workspace.lifecycle === "active");
  const active_workspace_ids = new Set(active_workspaces.map((workspace) => workspace.workspace_id));
  const visible_sessions = sessions.filter((session) => (
    session.role !== "controller"
    && (!session.workspace_id || active_workspace_ids.has(session.workspace_id))
  ));
  const next_offset = store.navigation.list_mode === "active" ? application?.active_sessions_next_offset : application?.archived_sessions_next_offset;
  const pinned = visible_sessions.filter((session) => session.is_pinned);
  const unpinned = visible_sessions.filter((session) => !session.is_pinned);
  const controller_id = application?.controller_availability.status === "available"
    ? application.controller_availability.session_id : null;
  const controller_session = application?.active_sessions.find((session) => session.session_id === controller_id);

  async function loadMore() {
    if (list_loading || next_offset == null) return;
    setListLoading(true);
    setListError(null);
    try {
      await store.loadMoreSessions(store.navigation.list_mode);
    } catch (error) {
      setListError(error instanceof Error ? error.message : "加载失败");
    } finally { setListLoading(false); }
  }
  const groups = active_workspaces
    .map((workspace) => ({
      workspace,
      sessions: unpinned.filter((session) => session.workspace_id === workspace.workspace_id),
    }));
  const unbound = unpinned.filter((session) => !session.workspace_id);

  useEffect(() => {
    const current_ids = new Set(visible_sessions.filter((session) => session.is_pinned).map((session) => session.session_id));
    for (const session of visible_sessions) {
      if (previous_pinned_ids.current?.has(session.session_id) && !current_ids.has(session.session_id)) {
        if (session.workspace_id) store.navigation.ensureWorkspaceExpanded(session.workspace_id);
        else setUnboundSectionOpen(true);
      }
    }
    previous_pinned_ids.current = current_ids;
  }, [store.navigation, visible_sessions]);

  return (
    <aside
      aria-label="会话导航"
      aria-hidden={hidden}
      className={styles.sidebar}
      data-shell-open={!hidden}
      data-shell-sidebar="left"
      inert={hidden}
      >
      <div className={styles.native_control_region} data-tauri-drag-region="deep">
        <SessionNavigation on_search={on_search} on_toggle={on_toggle} sidebar_open />
      </div>
      <div className={styles.primary_actions}>
        <Tooltip content="新建会话">
          <button
            aria-label="新建会话"
            aria-current={store.navigation.selected_draft_key === draftKeyForWorkspace(null) ? "page" : undefined}
            className={styles.quick_action}
            data-navigation-action
            disabled={store.connection.state !== "connected" || store.pending_session_action || store.pending_workspace_action}
            onClick={() => store.openNewSessionDraft(null)}
            type="button"
          ><Icon name="plus" size={17} /><span>新建会话</span></button>
        </Tooltip>
        <Tooltip content={controller_id ? "助理会话" : "助理会话暂不可用"}>
          <button
            aria-label="助理会话"
            aria-current={controller_id && store.navigation.selected_session_id === controller_id ? "page" : undefined}
            className={styles.quick_action}
            data-navigation-action
            disabled={!controller_id || store.connection.state !== "connected"}
            onClick={() => { if (controller_id) void store.selectSession(controller_id); }}
            title={controller_session?.title}
            type="button"
          ><Icon name="bot" size={17} /><span>助理会话</span></button>
        </Tooltip>
      </div>

      <div className={styles.session_scroll}>
          <>
            {pinned.length > 0 && <section aria-label="置顶会话" className={styles.pinned_section}>
              <div className={styles.section_header}>
                <button
                  aria-expanded={pinned_section_open}
                  className={styles.section_label}
                  onClick={() => setPinnedSectionOpen((open) => !open)}
                  type="button"
                >置顶</button>
                <button
                  aria-expanded={pinned_section_open}
                  aria-label={pinned_section_open ? "收起置顶" : "展开置顶"}
                  className={styles.section_chevron}
                  onClick={() => setPinnedSectionOpen((open) => !open)}
                  type="button"
                ><Icon name="chevron-down" size={14} /></button>
              </div>
              <Collapse open={pinned_section_open}>
                <SessionList indent="root" sessions={pinned} />
              </Collapse>
            </section>}
            <div className={`${styles.section_header} ${styles.workspace_section_header}`}>
              <button
                aria-expanded={workspace_section_open}
                className={styles.section_label}
                onClick={() => setWorkspaceSectionOpen((open) => !open)}
                type="button"
              >
                项目
              </button>
              <Tooltip content="添加项目">
                <button
                  aria-label="添加项目"
                  className={styles.section_action}
                  disabled={
                    store.connection.state !== "connected" ||
                    store.pending_workspace_action ||
                    store.pending_session_action
                  }
                  onClick={() => void store.addWorkspace()}
                  type="button"
                >
                  <Icon name="plus" size={14} />
                </button>
              </Tooltip>
              <Tooltip content="恢复已移除项目">
                <button
                  aria-label="恢复已移除项目"
                  className={styles.section_restore}
                  disabled={store.connection.state !== "connected" || store.pending_workspace_action}
                  onClick={() => setRestoreDialogOpen(true)}
                  type="button"
                ><Icon name="refresh" size={14} /></button>
              </Tooltip>
              <button
                aria-expanded={workspace_section_open}
                aria-label={workspace_section_open ? "收起项目" : "展开项目"}
                className={styles.section_chevron}
                onClick={() => setWorkspaceSectionOpen((open) => !open)}
                type="button"
              >
                <Icon name="chevron-down" size={14} />
              </button>
            </div>
            <Collapse open={workspace_section_open}>
              <div>
                {groups.map((group) => (
                  <WorkspaceGroup
                    key={group.workspace.workspace_id}
                    sessions={group.sessions}
                    workspace={group.workspace}
                  />
                ))}
              </div>
            </Collapse>
            {unbound.length > 0 && (
              <section className={styles.unbound_section}>
                <div className={styles.section_header}>
                  <button
                    aria-expanded={unbound_section_open}
                    className={styles.section_label}
                    onClick={() => setUnboundSectionOpen((open) => !open)}
                    type="button"
                  >
                    独立会话
                  </button>
                  {store.navigation.list_mode === "active" && (
                    <Tooltip content="新建独立会话">
                      <button
                        aria-label="新建独立会话"
                        className={styles.section_action}
                        disabled={
                          store.connection.state !== "connected" ||
                          store.pending_session_action ||
                          store.pending_workspace_action
                        }
                        onClick={() => store.openNewSessionDraft(null)}
                        type="button"
                      >
                        <Icon name="plus" size={14} />
                      </button>
                    </Tooltip>
                  )}
                  <button
                    aria-expanded={unbound_section_open}
                    aria-label={unbound_section_open ? "收起独立会话" : "展开独立会话"}
                    className={styles.section_chevron}
                    onClick={() => setUnboundSectionOpen((open) => !open)}
                    type="button"
                  >
                    <Icon name="chevron-down" size={14} />
                  </button>
                </div>
                <Collapse open={unbound_section_open}>
                  <SessionList indent="root" sessions={unbound} />
                </Collapse>
              </section>
            )}
            {sessions.length === 0 && <div className={styles.empty_list}>暂无会话</div>}
          </>
        {next_offset != null && <Button variant="text" size="small" disabled={list_loading} onClick={() => void loadMore()}>{list_loading ? "加载中…" : "加载更多"}</Button>}
        {list_error && <p role="alert">{list_error}</p>}
      </div>
      {application_connection && <AccountMenu connection={application_connection} sidebar />}
      <RemovedWorkspacesDialog on_close={() => setRestoreDialogOpen(false)} open={restore_dialog_open} />
    </aside>
  );
});
