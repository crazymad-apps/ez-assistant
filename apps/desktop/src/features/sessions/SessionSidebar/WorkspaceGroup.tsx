import { WORKSPACES, sessionScope } from "../../../stores/SidebarOrderStore";
import { ResetOrderDialog } from "./ResetOrderDialog";
import { SessionGroupPage } from "./SessionGroupPage";
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
import type { WorkspaceSummary } from "@ez-assistant/protocol";
import { useRootStore } from "../../../stores/RootStoreContext";
import { draftKeyForWorkspace } from "../../../stores/NewSessionDraftStore";
import { SessionActionDialog } from "../SessionActionDialog";
import styles from "./index.module.scss";

export const WorkspaceGroup = observer(function WorkspaceGroup(props: Readonly<{
  workspace: WorkspaceSummary;
}>) {
  const store = useRootStore();
  const [reset_open, setResetOpen] = useState(false);
  const order = store.sidebar_order;
  const scope = sessionScope(props.workspace.workspace_id);
  const dragging = order.dragging?.key === order.key(WORKSPACES) && order.dragging.item === props.workspace.workspace_id;
  const [remove_open, setRemoveOpen] = useState(false);
  const is_expanded = store.sidebar_navigation.expanded_workspaces.has(props.workspace.workspace_id);
  const duplicated = (store.sidebar_projection.application?.workspaces ?? []).filter((workspace) =>
    workspace.lifecycle === "active" && workspace.label === props.workspace.label
  ).length > 1;
  const name = `${props.workspace.label}${duplicated ? ` · ${props.workspace.workspace_id.slice(0, 8)}` : ""}`;


  async function removeWorkspace() {
    const removed = await store.removeWorkspace(props.workspace.workspace_id);
    if (removed) setRemoveOpen(false);
  }

  return (
    <section className={styles.workspace_group}>
      <div className={styles.workspace_row} data-sidebar-scope={JSON.stringify(WORKSPACES)} data-sidebar-item={props.workspace.workspace_id} aria-busy={order.isPending(WORKSPACES)}>
        <button
          aria-expanded={is_expanded}
          className={styles.workspace_toggle}
          data-sidebar-handle
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
              <DropdownMenuItem disabled={!order.enabled(scope)} onSelect={() => setResetOpen(true)}>
                <Icon name="sort" size={15} /><span>恢复默认顺序</span>
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
      <Collapse open={is_expanded && !dragging}>
        <SessionGroupPage workspace_id={props.workspace.workspace_id} open={is_expanded && !dragging && store.sidebar_navigation.workspace_section_open} />
      </Collapse>
      {reset_open && <ResetOrderDialog scope={scope} title={`恢复“${name}”内会话的默认顺序？`} on_close={() => setResetOpen(false)} />}
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
            <p>该项目的会话会一并从侧栏隐藏，重新添加此目录后恢复显示。</p>
          </SessionActionDialog>
        )}
      </PresenceBoundary>
    </section>
  );
});
