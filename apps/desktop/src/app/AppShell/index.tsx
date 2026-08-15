import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import { Icon } from "../../components/Icon";
import { Tooltip } from "../../components/Tooltip";
import { ContextPanel } from "../../features/context-panel/ContextPanel";
import { DesktopLifecycleDialog } from "../../features/desktop-lifecycle/DesktopLifecycleDialog";
import { ComposerDock } from "../../features/composer/ComposerDock";
import { ConversationView } from "../../features/conversation/ConversationView";
import { RuntimeStatus } from "../../features/runtime-status/RuntimeStatus";
import { SettingsDialog } from "../../features/settings/SettingsDialog";
import { SessionSidebar } from "../../features/sessions/SessionSidebar";
import {
  getDesktopPlatform,
  isDesktopWindowMaximized,
  listenDesktopWindowMaximized,
  minimizeDesktopWindow,
  requestDesktopClose,
  toggleMaximizeDesktopWindow,
  type DesktopPlatform,
} from "../../native-bridge/desktopLifecycle";
import { useRootStore } from "../../stores/RootStoreContext";
import { ChildTaskSubheader } from "./ChildTaskSubheader";
import { SessionHeader } from "./SessionHeader";
import styles from "./index.module.scss";

export const AppShell = observer(function AppShell() {
  const store = useRootStore();
  const [desktop_platform, setDesktopPlatform] = useState<DesktopPlatform>("unsupported");
  const [window_maximized, setWindowMaximized] = useState(false);
  const application = store.projection.application;
  const session_id = store.navigation.selected_session_id;
  const session = application?.active_sessions.find((item) => item.session_id === session_id)
    ?? application?.archived_sessions.find((item) => item.session_id === session_id);

  useEffect(() => {
    let active = true;
    let unlisten: () => void = () => undefined;
    void getDesktopPlatform().then((platform) => {
      if (!active) return;
      document.documentElement.dataset.platform = platform;
      setDesktopPlatform(platform);
      if (platform !== "linux") return;
      void isDesktopWindowMaximized().then((maximized) => {
        if (active) setWindowMaximized(maximized);
      });
      void listenDesktopWindowMaximized((maximized) => {
        if (active) setWindowMaximized(maximized);
      }).then((dispose) => {
        if (active) unlisten = dispose;
        else dispose();
      });
    });
    return () => {
      active = false;
      unlisten();
    };
  }, []);

  function handleTitleBarDoubleClick(event: React.MouseEvent<HTMLElement>) {
    if (desktop_platform !== "linux" || (event.target as HTMLElement).closest("button")) return;
    void toggleMaximizeDesktopWindow().then(setWindowMaximized);
  }

  return (
    <main
      className={styles.app_shell}
      data-left-sidebar={store.navigation.left_sidebar_open}
      data-right-sidebar={store.navigation.right_sidebar_open}
    >
      <header
        className={styles.title_bar}
        data-app-title-bar
        data-platform={desktop_platform}
        data-tauri-drag-region
        onDoubleClick={handleTitleBarDoubleClick}
      >
        <button
          aria-label={store.navigation.left_sidebar_open ? "收起会话栏" : "展开会话栏"}
          className={styles.title_icon}
          onClick={() => store.toggleLeftSidebar()}
          type="button"
        >
          <Icon name="menu" size={17} />
        </button>
        <strong className={styles.app_title} data-tauri-drag-region>
          <span data-tauri-drag-region>ez-assistant · 本地 AI 助手</span>
          <span className={styles.app_version} data-tauri-drag-region>v{__APP_VERSION__}</span>
        </strong>
        <RuntimeStatus />
        {desktop_platform === "linux" && <LinuxWindowControls maximized={window_maximized} on_maximized_change={setWindowMaximized} />}
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
      <SettingsDialog />
      <DesktopLifecycleDialog />
      <div id="overlay-root" />
    </main>
  );
});

function LinuxWindowControls(props: Readonly<{
  maximized: boolean;
  on_maximized_change: (maximized: boolean) => void;
}>) {
  return (
    <div className={styles.window_controls} aria-label="窗口控制">
      <Tooltip content="最小化">
        <button aria-label="最小化窗口" onClick={() => void minimizeDesktopWindow()} type="button">
          <span className={styles.minimize_icon} aria-hidden="true" />
        </button>
      </Tooltip>
      <Tooltip content={props.maximized ? "还原" : "最大化"}>
        <button
          aria-label={props.maximized ? "还原窗口" : "最大化窗口"}
          onClick={() => void toggleMaximizeDesktopWindow().then(props.on_maximized_change)}
          type="button"
        >
          <span className={props.maximized ? styles.restore_icon : styles.maximize_icon} aria-hidden="true" />
        </button>
      </Tooltip>
      <Tooltip content="关闭">
        <button
          aria-label="关闭窗口"
          className={styles.close_window}
          onClick={() => void requestDesktopClose()}
          type="button"
        >
          <Icon name="x" size={15} />
        </button>
      </Tooltip>
    </div>
  );
}
