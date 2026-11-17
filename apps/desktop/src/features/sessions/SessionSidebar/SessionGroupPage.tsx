import { sessionScope } from "../../../stores/SidebarOrderStore";
import { observer } from "mobx-react-lite";
import { useEffect } from "react";
import { useRootStore } from "../../../stores/RootStoreContext";
import { sessionGroupKey } from "../../../stores/RuntimeProjectionStore";
import { SessionList } from "./SessionList";
import { SidebarLoadMore } from "./SidebarLoadMore";
import styles from "./index.module.scss";

/** 分组未加载、空页与全部已置顶是三种不同状态；折叠时不发读取请求。 */
export const SessionGroupPage = observer(function SessionGroupPage(props: Readonly<{ workspace_id: string | null; open: boolean }>) {
  const store = useRootStore();
  const projection = store.sidebar_projection;
  const scope = sessionScope(props.workspace_id);
  const filter = store.sidebar_navigation.list_mode;
  const page = projection.sessionGroup(props.workspace_id, filter);
  const key = sessionGroupKey(props.workspace_id, filter);
  const state = projection.listLoadState(key);
  const revision = projection.list_revision;
  const target = store.network.browsing_device;
  const connected = store.sidebar_connection.state === "connected";
  useEffect(() => {
    if (props.open && connected && !page && state.status === "idle") void store.loadSessionGroup(props.workspace_id, filter);
  }, [store, props.open, props.workspace_id, filter, page, state.status, revision, target, connected]);
  if (!props.open) return null;
  const sessions = page?.sessions.filter(session => !store.isSessionPinned(session, target)) ?? [];
  const loading = state.status === "loading";
  return <div className={styles.group_page} data-indent={props.workspace_id === null ? "root" : "nested"}>
    {!page && !state.error && <div aria-label="正在加载会话" aria-busy="true" className={styles.group_skeleton}><span /><span /></div>}
    {page?.sessions.length === 0 && <p className={styles.group_empty}>暂无会话</p>}
    <SessionList scope={scope} indent={props.workspace_id === null ? "root" : "nested"} sessions={sessions} />
    {(page?.next_offset != null || (!page && state.error)) && (
      <SidebarLoadMore
        scope={scope}
        loading={loading}
        disabled={!connected}
        onLoad={() => void store.loadSessionGroup(props.workspace_id, filter, Boolean(page))}
      />
    )}
    {state.error && <p role="alert" className={styles.group_empty}>加载失败</p>}
  </div>;
});
