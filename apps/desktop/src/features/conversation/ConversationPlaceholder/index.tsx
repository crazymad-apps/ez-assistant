import { observer } from "mobx-react-lite";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { workspaceDisplayName } from "../../sessions/sessionFormatters";
import styles from "./index.module.scss";

export const ConversationPlaceholder = observer(function ConversationPlaceholder() {
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = application?.active_sessions.find((item) => item.session_id === session_id)
    ?? application?.archived_sessions.find((item) => item.session_id === session_id);
  const workspace = application?.workspaces.find(
    (item) => item.workspace_id === session?.workspace_id,
  );
  const first_active_workspace = application?.workspaces.find(
    (item) => item.lifecycle === "active",
  );

  if (!application) {
    const is_connecting = ["booting", "starting_runtime", "connecting", "reconnecting"].includes(
      store.connection.state,
    );
    return (
      <div className={styles.state_panel} role="status">
        <span className={styles.state_icon}><Icon name="message" size={22} /></span>
        <strong>{is_connecting ? "正在连接本地运行时" : "运行时暂不可用"}</strong>
        <span>{store.connection.error_message ?? "读取工作区与会话…"}</span>
      </div>
    );
  }

  if (!session) {
    return (
      <div className={styles.state_panel}>
        <span className={styles.state_icon}><Icon name="message" size={22} /></span>
        <strong>开始新会话</strong>
        <span>选择一个工作区新建会话。</span>
        <button
          disabled={
            store.connection.state !== "connected" ||
            store.pending_session_action ||
            store.pending_workspace_action
          }
          onClick={() => void store.createSession(first_active_workspace?.workspace_id)}
          type="button"
        >
          新对话
        </button>
      </div>
    );
  }

  return (
    <div className={styles.state_panel}>
      <span className={styles.state_icon}><Icon name="message" size={22} /></span>
      <strong>{session.message_count === 0 ? "开始新会话" : "会话已载入"}</strong>
      <span>
        {workspace ? `工作区 · ${workspaceDisplayName(workspace.user_directory)}` : "未绑定工作区"}
      </span>
      {session.message_count > 0 && <small>消息列表将在 M3 接入正式分页投影。</small>}
    </div>
  );
});
