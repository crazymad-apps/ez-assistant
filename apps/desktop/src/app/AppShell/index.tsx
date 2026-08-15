import { observer } from "mobx-react-lite";
import { Icon } from "../../components/Icon";
import { ContextPanel } from "../../features/context-panel/ContextPanel";
import { ComposerDock } from "../../features/composer/ComposerDock";
import { ConversationView } from "../../features/conversation/ConversationView";
import { RuntimeStatus } from "../../features/runtime-status/RuntimeStatus";
import { SessionSidebar } from "../../features/sessions/SessionSidebar";
import { useRootStore } from "../../stores/RootStoreContext";
import { ChildTaskSubheader } from "./ChildTaskSubheader";
import { SessionHeader } from "./SessionHeader";
import styles from "./index.module.scss";

export const AppShell = observer(function AppShell() {
  const store = useRootStore();
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = application?.active_sessions.find((item) => item.session_id === session_id)
    ?? application?.archived_sessions.find((item) => item.session_id === session_id);

  return (
    <main
      className={styles.app_shell}
      data-left-sidebar={store.navigation.left_sidebar_open}
      data-right-sidebar={store.navigation.right_sidebar_open}
    >
      <header className={styles.title_bar} data-app-title-bar data-tauri-drag-region>
        <button
          aria-label={store.navigation.left_sidebar_open ? "收起会话栏" : "展开会话栏"}
          className={styles.title_icon}
          onClick={() => store.toggleLeftSidebar()}
          type="button"
        >
          <Icon name="menu" size={17} />
        </button>
        <strong data-tauri-drag-region>ez-assistant · 本地 AI 助手</strong>
        <RuntimeStatus />
      </header>

      <div className={styles.app_body}>
        {store.navigation.left_sidebar_open && <SessionSidebar />}
        <section className={styles.conversation_area}>
          <div className={styles.conversation_headers}>
            <SessionHeader session={session} />
            <ChildTaskSubheader />
          </div>
          {store.connection.state !== "connected" && application && (
            <div className={styles.connection_notice} role="status">
              <span>{store.connection.error_message ?? "Runtime 连接不可用，当前内容为上次快照。"}</span>
              <button onClick={() => store.retryConnection()} type="button">重新连接</button>
            </div>
          )}
          <div className={styles.conversation_content}>
            <ConversationView />
          </div>
          <div className={styles.composer_slot}>
            <ComposerDock read_only={store.navigation.selected_child_task_id !== null} />
          </div>
        </section>
        {store.navigation.right_sidebar_open && <ContextPanel />}
      </div>
      <div id="overlay-root" />
    </main>
  );
});
