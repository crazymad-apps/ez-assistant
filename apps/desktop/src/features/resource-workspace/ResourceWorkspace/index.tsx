import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState, type KeyboardEvent, type WheelEvent, type RefObject } from "react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../../../components/DropdownMenu";
import { Button } from "../../../components/Button";
import { Icon, type IconName } from "../../../components/Icon";
import { ContextPanel } from "../../context-panel/ContextPanel";
import { useRootStore } from "../../../stores/RootStoreContext";
import {
  CONTEXT_TAB_KEY,
  resourceTabKey,
  resourceTabTitle,
  type ResourceTab,
} from "../ResourceWorkspaceStore";
import { ResourcePage } from "../ResourcePage";
import { CloseTerminalDialog } from "../ResourceTerminal/CloseTerminalDialog";
import { useBrowserSurface } from "../ResourceBrowser/useBrowserSurface";
import { ResourceAddMenu } from "../ResourceAddMenu";
import { supportsNativeResourceMenu } from "../../../native-bridge/resourceMenu";
import styles from "./index.module.scss";
import { shellBadges, shellLabels } from "../../../runtime-client/shellPresentation";

export const ResourceWorkspace = observer(function ResourceWorkspace(props: Readonly<{ hidden?: boolean; overlay_root_ref?: RefObject<HTMLDivElement | null> }>) {
  const root_store = useRootStore();
  const store = root_store.reading_resources;
  const owner = store.active_group;
  const viewports = useRef(new Map<string, HTMLDivElement>());
  const fallback_overlay = useRef<HTMLDivElement>(null);
  const active_browser_id = store.active_tab.type === "browser" ? store.active_tab.browserId : null;
  useBrowserSurface(active_browser_id ? store.browsers.get(active_browser_id) : undefined, !props.hidden && root_store.reading_connection.state === "connected",
    viewports, active_browser_id, props.overlay_root_ref ?? fallback_overlay);
  const scope_key = root_store.reading_scope;
  const session_id = scope_key.startsWith("session:") ? scope_key.slice(8) : null;
  const draft_key = scope_key.startsWith("draft:") ? scope_key.slice(6) : null;
  const application = root_store.reading_projection.application;
  const session_view = session_id ? root_store.reading_projection.session_views.get(session_id) : undefined;
  const draft = root_store.reading_drafts.get(draft_key as import("../../../stores/NewSessionDraftStore").NewSessionDraftKey | null);
  const draft_workspace = application?.workspaces.find((item) => item.workspace_id === draft?.workspace_id);
  const [closing_terminal, setClosingTerminal] = useState<string | null>(null);
  const connected = root_store.reading_connection.state === "connected";
  const terminal_available = connected && Boolean(session_view || (!session_id && draft_workspace));
  const desktop_device = root_store.network.online_devices.find((device) => device.id === root_store.reading_device && device.desktop_view);
  const desktop_available = Boolean(desktop_device && root_store.local_files.native_host);
  const openDesktop = () => { if (desktop_device && desktop_available) store.openDesktop(desktop_device.id, desktop_device.name, desktop_device.desktop_control); };
  const workspace_available = connected && Boolean(session_id || draft_workspace);
  const tab_refs = useRef(new Map<string, HTMLButtonElement>());
  useEffect(() => { setClosingTerminal(null); }, [scope_key]);

  useEffect(() => {
    const tab = tab_refs.current.get(store.active_tab_key);
    const reveal = () => tab?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
    reveal();
    const scroller = tab?.parentElement?.parentElement;
    if (!scroller || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(reveal);
    observer.observe(scroller);
    return () => observer.disconnect();
  }, [store, store.active_tab_key]);

  function focusTab(key: string) {
    const tab = tab_refs.current.get(key);
    if (tab) {
      tab.focus();
      return;
    }
    requestAnimationFrame(() => tab_refs.current.get(key)?.focus());
  }

  function openTerminal() {
    if (session_id && session_view) {
      const directory = session_view.workspace?.primary_directory;
      store.openTerminal({ type: "session", session_id, locator: {
        root: { type: directory ? "workspace_primary" : "session_private" }, relative_path: "",
      } }, owner);
    } else if (draft_workspace) {
      store.openTerminal({ type: "workspace", workspace_id: draft_workspace.workspace_id }, owner);
    }
  }

  async function closeTerminal(terminal_id: string) {
    const next = await store.closeTerminalTab(terminal_id);
    setClosingTerminal(null);
    if (store.active_group === owner) focusTab(next);
  }

  function requestClose(tab: ResourceTab) {
    if (tab.type === "terminal") {
      if (store.terminals.get(tab.terminalId)?.needs_close_confirmation) setClosingTerminal(tab.terminalId);
      else void closeTerminal(tab.terminalId).catch((failure: unknown) => root_store.showInteractionError(String(failure)));
    } else focusTab(store.closeTab(resourceTabKey(tab)));
  }

  function handleTabKeyDown(event: KeyboardEvent<HTMLButtonElement>, tab: ResourceTab) {
    const key = resourceTabKey(tab);
    let next_key: string | null = null;
    if (event.key === "ArrowLeft") next_key = store.moveFocus("previous");
    if (event.key === "ArrowRight") next_key = store.moveFocus("next");
    if (event.key === "Home") next_key = store.moveFocus("first");
    if (event.key === "End") next_key = store.moveFocus("last");
    if (next_key) {
      event.preventDefault();
      focusTab(next_key);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      store.activateTab(key);
      return;
    }
    if (event.key === "Delete" && key !== CONTEXT_TAB_KEY) {
      event.preventDefault();
      requestClose(tab);
    }
  }

  function handleTabWheel(event: WheelEvent<HTMLDivElement>) {
    const scroller = event.currentTarget;
    if (Math.abs(event.deltaY) <= Math.abs(event.deltaX) || scroller.scrollWidth <= scroller.clientWidth) return;
    event.preventDefault();
    scroller.scrollLeft += event.deltaY;
  }


  return (
    <aside className={styles.workspace} aria-label="资源栏" aria-hidden={props.hidden}
      data-shell-open={!props.hidden} data-shell-sidebar="right" inert={props.hidden}>
      {!connected && <div role="status" className={styles.scope_bar}>
        <span>{root_store.reading_connection.error_message ?? "原设备暂不可用"}</span>
        <Button size="small" variant="text" onClick={() => void root_store.connectReadingResources()}>重新连接</Button>
      </div>}
      <header className={styles.tab_bar} data-tauri-drag-region="deep">
        <div aria-label="资源标签" className={styles.tab_scroller} onWheel={handleTabWheel} role="tablist">
          {store.tabs.map((tab) => {
            const key = resourceTabKey(tab);
            const title = tab.type === "browser" ? store.browsers.get(tab.browserId)?.title ?? resourceTabTitle(tab) : tab.type === "terminal" ? store.terminals.get(tab.terminalId)?.title ?? resourceTabTitle(tab) : resourceTabTitle(tab);
            const active = store.active_tab_key === key;
            const shell = tab.type === "terminal" ? store.terminals.get(tab.terminalId)?.shell_kind : null;
            return (
              <div
                className={styles.tab_item}
                data-active={active}
                key={key}
                onAuxClick={(event) => {
                  if (event.button !== 1) return;
                  event.preventDefault();
                  if (key !== CONTEXT_TAB_KEY) requestClose(tab);
                }}
                onMouseDown={(event) => {
                  if (event.button === 1) event.preventDefault();
                }}
                role="presentation"
              >
                <button
                  aria-controls={tabPanelId(tab.type === "context" ? key : store.pageKey(store.active_group, tab))}
                  aria-selected={active}
                  className={styles.tab}
                  id={`resource-tab-${encodeURIComponent(key)}`}
                  onClick={() => store.activateTab(key)}
                  onFocus={() => store.focusTab(key)}
                  onKeyDown={(event) => handleTabKeyDown(event, tab)}
                  ref={(element) => {
                    if (element) tab_refs.current.set(key, element);
                    else tab_refs.current.delete(key);
                  }}
                  role="tab"
                  tabIndex={store.focused_tab_key === key ? 0 : -1}
                  type="button"
                >
                  <Icon name={tabIcon(tab)} size={15} />
                  <span>{title}</span>
                  {shell && <small title={shellLabels[shell]}>{shellBadges[shell]}</small>}
                </button>
                {key !== CONTEXT_TAB_KEY && (
                  <Button
                    aria-label={`关闭 ${title}`}
                    className={styles.close_tab}
                    iconOnly
                    onClick={() => requestClose(tab)}
                    size="small"
                    variant="text"
                  >
                    <Icon name="x" size={13} />
                  </Button>
                )}
              </div>
            );
          })}
        </div>
        {supportsNativeResourceMenu() ? <ResourceAddMenu
          workspace_available={workspace_available}
          terminal_available={terminal_available}
          on_terminal={openTerminal}
          desktop_available={desktop_available}
          on_desktop={openDesktop}
          on_workspace={() => { if (scope_key && workspace_available) store.openWorkspace(scope_key); }}
          on_browser={() => store.openBrowser(undefined, owner)}
          on_error={(failure) => root_store.showInteractionError(String(failure))}
        /> : <DropdownMenu className={styles.add_menu} key={owner.id}>
          <DropdownMenuTrigger aria-label="新建资源标签" iconOnly variant="text">
            <Icon name="plus" size={17} />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" aria-label="新建资源标签">
            <ResourceMenuItem
              disabled={!workspace_available}
              icon="folder"
              label="项目"
              on_select={() => scope_key && workspace_available && store.openWorkspace(scope_key)}
            />
            <ResourceMenuItem disabled={!terminal_available} icon="terminal" label="终端" on_select={openTerminal} />
            <ResourceMenuItem disabled={!desktop_available} icon="desktop" label="远程桌面" on_select={openDesktop} />
          </DropdownMenuContent>
        </DropdownMenu>}
      </header>
      {connected && <div className={styles.content}>
        {store.browser_error && <p role="alert">{store.browser_error}</p>}
        <section className={styles.tab_panel} hidden={store.active_tab_key !== CONTEXT_TAB_KEY}
          aria-labelledby="resource-tab-context" id={tabPanelId(CONTEXT_TAB_KEY)} role="tabpanel">
          <ContextPanel embedded />
        </section>
        {store.mounted_pages.map((page) => {
          const key = resourceTabKey(page.tab);
          const active = page.group === store.active_group && store.active_tab_key === key;
          return <section className={styles.tab_panel} hidden={!active} key={page.key}
            aria-labelledby={active ? `resource-tab-${encodeURIComponent(key)}` : undefined}
            id={tabPanelId(page.key)} role="tabpanel">
            <ResourcePage page={page} active={active && !props.hidden} viewports={viewports} />
          </section>;
        })}
      </div>}
      {closing_terminal && <CloseTerminalDialog title={store.terminals.get(closing_terminal)?.title ?? "终端"}
        on_cancel={() => setClosingTerminal(null)} on_confirm={() => closeTerminal(closing_terminal)} />}

    </aside>
  );
});

function tabPanelId(key: string): string {
  return `resource-panel-${encodeURIComponent(key)}`;
}

function ResourceMenuItem(props: Readonly<{
  disabled: boolean;
  icon: IconName;
  label: string;
  on_select?: () => void;
}>) {
  return (
    <DropdownMenuItem className={styles.menu_item} disabled={props.disabled} onSelect={props.on_select}>
      <Icon name={props.icon} size={16} />
      <span>{props.label}</span>
    </DropdownMenuItem>
  );
}

function tabIcon(tab: ResourceTab): IconName {
  switch (tab.type) {
    case "context": return "sidebar-right";
    case "workspace": return "folder";
    case "text":
    case "markdown": return "file";
    case "image": return "image";
    case "pdf": return "file";
    case "browser": return "globe";
    case "terminal": return "terminal";
    case "desktop": return "desktop";
  }
}
