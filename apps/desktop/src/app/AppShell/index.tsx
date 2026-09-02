import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState, type CSSProperties } from "react";
import { Icon } from "../../components/Icon";
import { Tooltip } from "../../components/Tooltip";
import { usePresence } from "../../components/Presence";
import { ContextPanel } from "../../features/context-panel/ContextPanel";
import { DesktopLifecycleDialog } from "../../features/desktop-lifecycle/DesktopLifecycleDialog";
import { ComposerDock } from "../../features/composer/ComposerDock";
import { ConversationView } from "../../features/conversation/ConversationView";
import { RuntimeStatus } from "../../features/runtime-status/RuntimeStatus";
import { SettingsDialog } from "../../features/settings/SettingsDialog";
import { SessionSidebar } from "../../features/sessions/SessionSidebar";
import { ConversationSearchDialog } from "../../features/sessions/ConversationSearchDialog";
import { WorkspaceEditorDialog } from "../../features/workspaces/WorkspaceEditorDialog";
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
import {
  LEFT_SIDEBAR_DEFAULT_WIDTH,
  LEFT_SIDEBAR_MIN_WIDTH,
  RIGHT_SIDEBAR_DEFAULT_WIDTH,
  RIGHT_SIDEBAR_MIN_WIDTH,
} from "../../stores/NavigationStore";
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
  const navigation = store.navigation;
  const workspace_editor_presence = usePresence(store.workspace_editor !== null, 120);
  const retained_workspace_editor_ref = useRef(store.workspace_editor);
  if (store.workspace_editor) retained_workspace_editor_ref.current = store.workspace_editor;

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

  useEffect(() => {
    function updateViewportWidth() {
      navigation.setViewportWidth(Math.max(document.documentElement.clientWidth, window.innerWidth));
    }
    updateViewportWidth();
    window.addEventListener("resize", updateViewportWidth);
    return () => window.removeEventListener("resize", updateViewportWidth);
  }, [navigation]);

  function handleTitleBarDoubleClick(event: React.MouseEvent<HTMLElement>) {
    if (desktop_platform !== "linux" || (event.target as HTMLElement).closest("button")) return;
    void toggleMaximizeDesktopWindow().then(setWindowMaximized);
  }

  return (
    <main
      className={styles.app_shell}
      data-left-sidebar={navigation.effective_left_sidebar_open}
      data-right-sidebar={navigation.effective_right_sidebar_open}
      style={{
        "--ez-left-sidebar-width": `${navigation.effective_left_sidebar_width}px`,
        "--ez-right-sidebar-width": `${navigation.effective_right_sidebar_width}px`,
      } as CSSProperties}
    >
      <header
        className={styles.title_bar}
        data-app-title-bar
        data-platform={desktop_platform}
        data-tauri-drag-region
        onDoubleClick={handleTitleBarDoubleClick}
      >
        <button
          aria-label={navigation.effective_left_sidebar_open ? "收起会话栏" : "展开会话栏"}
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
        {navigation.effective_left_sidebar_open && <SessionSidebar />}
        {navigation.effective_left_sidebar_open && <SidebarResizeHandle side="left" />}
        <section className={styles.conversation_area} data-conversation-area>
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
        {navigation.effective_right_sidebar_open && <SidebarResizeHandle side="right" />}
        {navigation.effective_right_sidebar_open && <ContextPanel />}
      </div>
      <SettingsDialog />
      <DesktopLifecycleDialog />
      <ConversationSearchDialog />
      {workspace_editor_presence.mounted && retained_workspace_editor_ref.current && (
        <WorkspaceEditorDialog
          editor={retained_workspace_editor_ref.current}
          key={retained_workspace_editor_ref.current.mode === "create"
            ? `create:${retained_workspace_editor_ref.current.primary_directory}`
            : `edit:${retained_workspace_editor_ref.current.workspace_id}`}
          on_exit_transition_end={workspace_editor_presence.onTransitionEnd}
          presence_state={workspace_editor_presence.state}
        />
      )}
      <div id="overlay-root" />
    </main>
  );
});

const SidebarResizeHandle = observer(function SidebarResizeHandle(props: Readonly<{
  side: "left" | "right";
}>) {
  const store = useRootStore();
  const navigation = store.navigation;
  const handle_ref = useRef<HTMLDivElement>(null);
  const drag_ref = useRef<Readonly<{
    pointer_id: number;
    start_x: number;
    start_width: number;
  }> | null>(null);
  const [dragging, setDragging] = useState(false);
  const [announcement, setAnnouncement] = useState("");
  const width = props.side === "left" ? navigation.effective_left_sidebar_width : navigation.effective_right_sidebar_width;
  const minimum = props.side === "left" ? LEFT_SIDEBAR_MIN_WIDTH : RIGHT_SIDEBAR_MIN_WIDTH;
  const maximum = props.side === "left"
    ? navigation.left_sidebar_current_max_width
    : navigation.right_sidebar_current_max_width;
  const label = props.side === "left" ? "会话栏" : "当前上下文栏";

  function announce(next_width: number) {
    setAnnouncement(`${label}宽度 ${Math.round(next_width)} 像素`);
  }

  function finishDrag() {
    const drag = drag_ref.current;
    if (!drag) return;
    drag_ref.current = null;
    setDragging(false);
    delete document.documentElement.dataset.sidebarResizing;
    const handle = handle_ref.current;
    if (handle?.hasPointerCapture(drag.pointer_id)) {
      handle.releasePointerCapture(drag.pointer_id);
    }
    const next_width = props.side === "left"
      ? navigation.left_sidebar_width
      : navigation.right_sidebar_width;
    store.setSidebarWidth(props.side, next_width, true);
    announce(next_width);
  }

  useEffect(() => {
    if (!dragging) return undefined;
    window.addEventListener("blur", finishDrag);
    return () => window.removeEventListener("blur", finishDrag);
  });

  useEffect(() => () => {
    const drag = drag_ref.current;
    if (!drag) return;
    drag_ref.current = null;
    delete document.documentElement.dataset.sidebarResizing;
    const handle = handle_ref.current;
    if (handle?.hasPointerCapture(drag.pointer_id)) handle.releasePointerCapture(drag.pointer_id);
  }, []);

  function updateFromPointer(client_x: number) {
    const drag = drag_ref.current;
    if (!drag) return;
    const delta = client_x - drag.start_x;
    store.setSidebarWidth(props.side, drag.start_width + (props.side === "left" ? delta : -delta));
  }

  function updateFromKeyboard(event: React.KeyboardEvent<HTMLDivElement>) {
    const step = event.shiftKey ? 24 : 8;
    let next_width: number | null = null;
    if (event.key === "Home") next_width = minimum;
    if (event.key === "End") next_width = maximum;
    if (event.key === "ArrowLeft") next_width = width + (props.side === "left" ? -step : step);
    if (event.key === "ArrowRight") next_width = width + (props.side === "left" ? step : -step);
    if (next_width === null) return;
    event.preventDefault();
    store.setSidebarWidth(props.side, next_width, true);
    announce(props.side === "left" ? navigation.left_sidebar_width : navigation.right_sidebar_width);
  }

  return (
    <>
      <div
        aria-label={`调整${label}宽度`}
        aria-orientation="vertical"
        aria-valuemax={maximum}
        aria-valuemin={minimum}
        aria-valuenow={width}
        className={styles.sidebar_resize_handle}
        data-dragging={dragging}
        data-side={props.side}
        onDoubleClick={() => {
          store.resetSidebarWidth(props.side);
          announce(props.side === "left" ? LEFT_SIDEBAR_DEFAULT_WIDTH : RIGHT_SIDEBAR_DEFAULT_WIDTH);
        }}
        onKeyDown={updateFromKeyboard}
        onLostPointerCapture={finishDrag}
        onPointerCancel={finishDrag}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          event.preventDefault();
          drag_ref.current = { pointer_id: event.pointerId, start_x: event.clientX, start_width: width };
          event.currentTarget.setPointerCapture(event.pointerId);
          document.documentElement.dataset.sidebarResizing = props.side;
          setDragging(true);
        }}
        onPointerMove={(event) => updateFromPointer(event.clientX)}
        onPointerUp={finishDrag}
        ref={handle_ref}
        role="separator"
        tabIndex={0}
      />
      <span aria-live="polite" className={styles.sr_only} role="status">{announcement}</span>
    </>
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
