import { PINS, WORKSPACES, sessionScope } from "../../../stores/SidebarOrderStore";
import type { ResetSidebarOrderRequest } from "@ez-assistant/protocol";
import { ResetOrderDialog } from "./ResetOrderDialog";
import { useSidebarDrag } from "./useSidebarDrag";
import { DesktopPins } from "./DesktopPins";
import { DeviceSelector } from "../DeviceSelector";
import { observer } from "mobx-react-lite";
import { useState } from "react";
import { Icon } from "../../../components/Icon";
import { Collapse } from "../../../components/Collapse";
import { Tooltip } from "../../../components/Tooltip";
import { useRootStore } from "../../../stores/RootStoreContext";
import { draftKeyForWorkspace } from "../../../stores/NewSessionDraftStore";
import { SessionGroupPage } from "./SessionGroupPage";
import { SidebarLoadMore } from "./SidebarLoadMore";
import { WorkspaceGroup } from "./WorkspaceGroup";
import { SessionList } from "./SessionList";
import { AccountMenu } from "../../runtime-access/AccountMenu";
import { useApplicationConnection } from "../../runtime-access/ApplicationConnectionContext";
import { SessionNavigation } from "../SessionNavigation";
import { RemovedWorkspacesDialog } from "../../workspaces/RemovedWorkspacesDialog";
import styles from "./index.module.scss";

export const SessionSidebar = observer(function SessionSidebar({ hidden = false, on_search, on_toggle, schedules_open, on_schedules, on_conversation }: Readonly<{
  hidden?: boolean;
  on_search: () => void;
  on_toggle: () => void;
  schedules_open?: boolean;
  on_schedules?: () => void;
  on_conversation?: () => void;
}>) {
  const store = useRootStore();
  const scroll_ref = useSidebarDrag(store);
  const [reset_scope, setResetScope] = useState<ResetSidebarOrderRequest | null>(null);
  const order = store.sidebar_order;
  const application_connection = useApplicationConnection();
  const application = store.sidebar_projection.application;
  const local_application = store.local_projection.application;
  const [pinned_section_open, setPinnedSectionOpen] = useState(true);
  const workspace_section_open = store.sidebar_navigation.workspace_section_open;
  const setWorkspaceSectionOpen = () => store.sidebar_navigation.setSectionOpen("workspaces", !workspace_section_open);
  const [restore_dialog_open, setRestoreDialogOpen] = useState(false);
  const unbound_section_open = store.sidebar_navigation.unbound_section_open;
  const setUnboundSectionOpen = () => store.sidebar_navigation.setSectionOpen("unbound", !unbound_section_open);
  const workspaces = order.rows(WORKSPACES, application?.workspaces.filter(workspace => workspace.lifecycle === "active") ?? [], (workspace) => workspace.workspace_id);
  const list_state = store.sidebar_projection.listLoadState("workspaces");
  const pinned = local_application?.pinned_sessions ?? [];
  const controller_id = local_application?.controller_availability.status === "available"
    ? local_application.controller_availability.session_id : null;

  return (
    <aside
      aria-label="会话导航"
      aria-hidden={hidden}
      className={styles.sidebar}
      data-shell-open={!hidden}
      data-shell-sidebar="left"
      onClickCapture={(event) => {
        if (event.target instanceof Element && event.target.closest(
          "[data-navigation-action]:not([data-schedule-navigation])",
        )) on_conversation?.();
      }}
      inert={hidden}
      >
      <div className={styles.native_control_region} data-tauri-drag-region="deep">
        <SessionNavigation on_search={on_search} on_toggle={on_toggle} sidebar_open />
      </div>
      <div className={styles.primary_actions}>
        <button
          aria-label="新建会话"
          aria-current={!schedules_open && store.navigation.selected_draft_key === draftKeyForWorkspace(null) ? "page" : undefined}
          className={styles.quick_action}
          data-navigation-action
          disabled={store.local_connection.state !== "connected" || store.pending_session_action || store.pending_workspace_action}
          onClick={() => store.openNewSessionDraft(null, store.network.browsing_device)}
          type="button"
        ><Icon name="plus" size={17} /><span>新建会话</span></button>
        <button
          aria-label="助理会话"
          aria-current={!schedules_open && controller_id && store.navigation.selected_session_id === controller_id ? "page" : undefined}
          className={styles.quick_action}
          data-navigation-action
          disabled={!controller_id || store.local_connection.state !== "connected"}
          onClick={() => { if (controller_id) void store.selectSession(controller_id, null); }}
          type="button"
        ><Icon name="bot" size={17} /><span>助理会话</span></button>
        {on_schedules && <button
          aria-current={schedules_open ? "page" : undefined}
          className={styles.quick_action}
          data-navigation-action
          data-schedule-navigation
          onClick={on_schedules}
          type="button"
        ><Icon name="clock" size={17} /><span>定时任务</span></button>}
        <button
          aria-current={store.settings.is_open && store.settings.page === "agents" ? "page" : undefined}
          className={styles.quick_action}
          data-navigation-action
          onClick={() => store.settings.open("agents")}
          type="button"
        >
          <Icon name="bot" size={17} /><span>智能体管理</span>
        </button>
        <button
          aria-current={store.settings.is_open && store.settings.page === "plugins" ? "page" : undefined}
          className={styles.quick_action}
          data-navigation-action
          onClick={() => store.settings.open("plugins")}
          type="button"
        >
          <Icon name="plugin" size={17} /><span>插件管理</span>
        </button>
      </div>

      {order.visibleError && <p role="alert" className={styles.order_error}>{order.visibleError}</p>}
      <div className={styles.session_scroll} ref={scroll_ref}>
          <>
            {(store.local_files.desktop ? store.pins.value.items.length > 0 || !!store.pins.error : pinned.length > 0) && <section aria-label="置顶会话" className={styles.pinned_section}>
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
                {store.local_files.desktop ? <DesktopPins /> : <SessionList indent="root" sessions={pinned} target_device={null} scope={PINS} />}
              </Collapse>
            </section>}
            <DeviceSelector />
            <div className={styles.section_header}>
              <button
                aria-expanded={workspace_section_open}
                className={styles.section_label}
                onClick={() => setWorkspaceSectionOpen()}
                type="button"
              >
                项目
              </button>
              <Tooltip content="添加项目">
                <button
                  aria-label="添加项目"
                  className={styles.section_action}
                  disabled={
                    store.sidebar_connection.state !== "connected" ||
                    store.pending_workspace_action ||
                    store.pending_session_action
                  }
                  onClick={() => void store.addWorkspace(store.network.browsing_device)}
                  type="button"
                >
                  <Icon name="plus" size={14} />
                </button>
              </Tooltip>
              <Tooltip content="恢复已移除项目">
                <button
                  aria-label="恢复已移除项目"
                  className={styles.section_restore}
                  disabled={store.sidebar_connection.state !== "connected" || store.pending_workspace_action}
                  onClick={() => setRestoreDialogOpen(true)}
                  type="button"
                ><Icon name="refresh" size={14} /></button>
              </Tooltip>
              {store.sidebar_navigation.list_mode === "active" && <Tooltip content="恢复默认顺序">
                <button type="button" aria-label="恢复项目默认顺序" className={styles.section_action} disabled={!order.enabled(WORKSPACES)} onClick={() => setResetScope({ scope: "workspaces" })}><Icon name="sort" size={14} /></button>
              </Tooltip>}
              <button
                aria-expanded={workspace_section_open}
                aria-label={workspace_section_open ? "收起项目" : "展开项目"}
                className={styles.section_chevron}
                onClick={() => setWorkspaceSectionOpen()}
                type="button"
              >
                <Icon name="chevron-down" size={14} />
              </button>
            </div>
            <Collapse open={workspace_section_open}>
              <div>
                {workspaces.map((workspace) => (
                  <WorkspaceGroup key={`${store.network.browsing_device ?? "local"}:${workspace.workspace_id}`} workspace={workspace} />
                ))}
                {application?.workspaces_next_offset != null && (
                  <SidebarLoadMore
                    scope={WORKSPACES}
                    loading={list_state.status === "loading"}
                    onLoad={() => void store.loadMoreWorkspaces()}
                  />
                )}
                {list_state.error && <p role="alert">加载失败</p>}
              </div>
            </Collapse>
            {(workspaces.length > 0 || !store.sidebar_projection.sessionGroup(null, store.sidebar_navigation.list_mode) || (store.sidebar_projection.sessionGroup(null, store.sidebar_navigation.list_mode)?.sessions.length ?? 0) > 0) && (
              <section className={styles.unbound_section}>
                <div className={styles.section_header}>
                  <button
                    aria-expanded={unbound_section_open}
                    className={styles.section_label}
                    onClick={() => setUnboundSectionOpen()}
                    type="button"
                  >
                    独立会话
                  </button>
                  {store.sidebar_navigation.list_mode === "active" && (
                    <Tooltip content="新建独立会话">
                      <button
                        aria-label="新建独立会话"
                        className={styles.section_action}
                        disabled={
                          store.sidebar_connection.state !== "connected" ||
                          store.pending_session_action ||
                          store.pending_workspace_action
                        }
                        onClick={() => store.openNewSessionDraft(null, store.network.browsing_device)}
                        type="button"
                      >
                        <Icon name="plus" size={14} />
                      </button>
                    </Tooltip>
                  )}
                  {store.sidebar_navigation.list_mode === "active" && <Tooltip content="恢复默认顺序">
                    <button type="button" aria-label="恢复独立会话默认顺序" className={styles.section_action} disabled={!order.enabled(sessionScope(null))} onClick={() => setResetScope(sessionScope(null))}><Icon name="sort" size={14} /></button>
                  </Tooltip>}
                  <button
                    aria-expanded={unbound_section_open}
                    aria-label={unbound_section_open ? "收起独立会话" : "展开独立会话"}
                    className={styles.section_chevron}
                    onClick={() => setUnboundSectionOpen()}
                    type="button"
                  >
                    <Icon name="chevron-down" size={14} />
                  </button>
                </div>
                <Collapse open={unbound_section_open}>
                  <SessionGroupPage workspace_id={null} open={unbound_section_open} />
                </Collapse>
              </section>
            )}
            {store.sidebar_connection.state === "connected" && workspaces.length === 0 && store.sidebar_projection.sessionGroup(null, store.sidebar_navigation.list_mode)?.sessions.length === 0 && <div className={styles.empty_list}>暂无会话</div>}
          </>
      </div>
      {application_connection && <AccountMenu connection={application_connection} sidebar />}
      {reset_scope && <ResetOrderDialog scope={reset_scope} title={reset_scope.scope === "workspaces" ? "恢复项目的默认顺序？" : "恢复独立会话的默认顺序？"} on_close={() => setResetScope(null)} />}
      <RemovedWorkspacesDialog key={store.network.browsing_device ?? "local"} on_close={() => setRestoreDialogOpen(false)} open={restore_dialog_open} />
    </aside>
  );
});
