import type { SidebarScope } from "../../../stores/SidebarOrderStore";
import { observer } from "mobx-react-lite";
import type { SessionSummary } from "@ez-assistant/protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { sessionTime } from "../sessionFormatters";
import styles from "./index.module.scss";

export const SessionList = observer(function SessionList(props: Readonly<{
  scope?: SidebarScope;
  target_device?: string | null;
  id?: string;
  indent?: "nested" | "root";
  sessions: readonly SessionSummary[];
}>) {
  const store = useRootStore();
  const target = props.target_device === undefined ? store.network.browsing_device : props.target_device;
  const execution = target ? store.network.contexts.get(target)?.live_execution : store.local_live_execution;

  const sessions = props.scope ? store.sidebar_order.rows(props.scope, props.sessions, (session) => session.session_id) : props.sessions;

  return (
    <div className={styles.session_list} data-indent={props.indent ?? "nested"} id={props.id}>
      {sessions.map((session) => (
        <div
          className={styles.session_row}
          data-sidebar-scope={props.scope ? JSON.stringify(props.scope) : undefined}
          data-sidebar-item={props.scope ? session.session_id : undefined}
          aria-busy={props.scope ? store.sidebar_order.isPending(props.scope) : undefined}
          data-has-actions={store.sidebar_navigation.list_mode === "active"}
          data-selected={store.navigation.target_device === target && store.navigation.selected_session_id === session.session_id}
          key={session.session_id}
        >
          <button
            aria-current={store.navigation.target_device === target && store.navigation.selected_session_id === session.session_id ? "page" : undefined}
            className={styles.session_open}
            data-navigation-action
            data-sidebar-handle
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
          {execution?.activeRunId(session.session_id, session.active_run_id) ? (
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
