import { observer } from "mobx-react-lite";
import { useEffect, useState, type RefObject } from "react";
import type { SessionResourceEntry } from "../../../generated/assistant-protocol";
import type { SessionResourceLocator } from "../../../generated/assistant-protocol";
import type { NewSessionDraftKey } from "../../../stores/NewSessionDraftStore";
import { useRootStore } from "../../../stores/RootStoreContext";
import { openSessionResourceInSystem, revealSessionResourceInDirectory } from "../../../native-bridge/nativeResource";
import { SessionResourceTree, type SessionResourceRootItem } from "../SessionResourceTree";
import { ResourcePreview } from "../ResourcePreview";
import { ResourceTerminal } from "../ResourceTerminal";
import { ResourceBrowser } from "../ResourceBrowser";
import { ResourceContextMenu, type ResourceMenuItem, type ResourceMenuLocation } from "../ResourceContextMenu";
import { isPreviewableResource, type CachedResourcePage } from "../ResourceWorkspaceStore";

/** 内容按标签 owner 取根目录；后台保活页面不能改用当前会话的投影或打开回调。 */
export const ResourcePage = observer(function ResourcePage(props: Readonly<{
  page: CachedResourcePage;
  active: boolean;
  viewports: RefObject<Map<string, HTMLDivElement>>;
}>) {
  const root_store = useRootStore();
  const store = root_store.resource_workspace;
  const { group, tab, key } = props.page;
  const { active, viewports } = props;
  const scope_key = group.scope_key;
  const session_id = scope_key.startsWith("session:") ? scope_key.slice(8) : null;
  const session_view = session_id ? root_store.projection.session_views.get(session_id) : undefined;
  const draft = scope_key.startsWith("draft:") ? root_store.new_session_drafts.get(scope_key.slice(6) as NewSessionDraftKey) : undefined;
  const workspace = root_store.projection.application?.workspaces.find((item) => item.workspace_id === draft?.workspace_id);
  const view_state = store.view_states.get(key)!;
  const roots = session_view ? sessionRoots(session_view.workspace)
    : workspace ? draftRoots(workspace.user_directory, workspace.additional_directories)
      : view_state.roots ?? (session_id ? sessionRoots(undefined) : []);
  useEffect(() => {
    // Host 重连会暂时清掉投影；保留根的展示信息，不把已有页面改成另一个根。
    if (session_view || workspace) view_state.roots = roots;
  }, [roots, session_view, workspace, view_state]);
  const [resource_menu, setResourceMenu] = useState<Readonly<{ items: readonly ResourceMenuItem[]; location: ResourceMenuLocation }> | null>(null);
  useEffect(() => { if (!active) setResourceMenu(null); }, [active]);
  function runResourceAction(request: Promise<void>, fallback: string) {
    void request.catch((failure: unknown) => {
      root_store.showInteractionError(failure instanceof Error ? failure.message : fallback);
    });
  }

  function openTreeResourceMenu(entry: SessionResourceEntry, location: ResourceMenuLocation) {
    if (!session_id || !scope_key) return;
    const directory = entry.kind === "directory";
    setResourceMenu({
      location,
      items: [
        {
          disabled: !directory && !isPreviewableResource(entry.display_name),
          label: directory ? "在工作空间中打开" : "在资源栏打开",
          on_select: () => {
            if (directory) store.openWorkspace(scope_key, entry.locator);
            else store.openSessionResource(scope_key, session_id, entry.locator, entry.display_name);
          },
        },
        {
          label: "使用系统应用打开",
          on_select: () => runResourceAction(
            openSessionResourceInSystem(session_id, entry.locator),
            "无法使用系统应用打开。",
          ),
        },
        {
          label: "在 Finder 中显示",
          on_select: () => runResourceAction(
            revealSessionResourceInDirectory(session_id, entry.locator),
            "无法在 Finder 中显示。",
          ),
        },
      ],
    });
  }

  function renderTabContent() {
    if (tab.type === "terminal") {
      const controller = store.terminals.get(tab.terminalId);
      if (controller) return <ResourceTerminal controller={controller} active={active} />;
    }
    if (tab.type === "browser") {
      const controller = store.browsers.get(tab.browserId);
      if (controller) return <ResourceBrowser controller={controller} active={active}
        viewport_ref={(element) => {
          if (element) viewports.current.set(tab.browserId, element);
          else viewports.current.delete(tab.browserId);
        }} />;
    }

    if (tab.type === "workspace") {
      return (
        <SessionResourceTree
          view_state={view_state}
          focus_locator={store.workspace_locations.get(group.scope_key) ?? null}
          on_open_file={(entry) => {
            if (session_id) {
              store.openSessionResource(group.scope_key, session_id, entry.locator, entry.display_name);
            }
          }}
          on_open_resource_menu={openTreeResourceMenu}
          roots={roots}
          session_id={session_id}
        />
      );
    }
    if (tab.type === "text" || tab.type === "markdown" || tab.type === "image" || tab.type === "pdf") {
      const tab_session_id = tab.resource.source.type === "session_file"
        ? tab.resource.source.session_id
        : session_id;
      return (
        <ResourcePreview
          active={active}
          view_state={view_state}
          on_focus_workspace={(resource_scope_key, locator) => store.openWorkspace(resource_scope_key, locator)}
          on_open_attachment={(attachment, siblings) => {
            store.openAttachment(group.scope_key, attachment, siblings);
          }}
          on_open_file={(entry) => {
            if (tab_session_id) {
              store.openSessionResource(group.scope_key, tab_session_id, entry.locator, entry.display_name);
            }
          }}
          on_open_local_resource={(resource) => {
            store.openLocalResource(group.scope_key, resource);
          }}
          on_open_tool_resource={(owner, message_id, file, siblings) => {
            store.openToolResource(group.scope_key, owner, message_id, file, siblings);
          }}
          roots={roots}
          tab={tab}
        />
      );
    }
    return null;
  }

  return <>{renderTabContent()}{active && resource_menu && <ResourceContextMenu
    items={resource_menu.items} location={resource_menu.location} on_close={() => setResourceMenu(null)} />}</>;
});

function sessionRoots(workspace: Readonly<{
  primary_directory: string;
  additional_directories: readonly string[];
}> | undefined): SessionResourceRootItem[] {
  const roots: SessionResourceRootItem[] = [];
  if (workspace) {
    roots.push(resourceRoot(
      "workspace-primary",
      workspace.primary_directory,
      "主目录",
      { root: { type: "workspace_primary" }, relative_path: "" },
    ));
    workspace.additional_directories.forEach((directory, directory_index) => {
      roots.push(resourceRoot(
        `workspace-additional-${directory_index}`,
        directory,
        `附加目录 ${directory_index + 1}`,
        { root: { type: "workspace_additional", directory_index }, relative_path: "" },
      ));
    });
  }
  roots.push({
    id: "session-private",
    label: "会话私有目录",
    detail: "当前会话",
    locator: { root: { type: "session_private" }, relative_path: "" },
  });
  return roots;
}

function draftRoots(primary: string, additional: readonly string[]): SessionResourceRootItem[] {
  return [primary, ...additional].map((directory, index) => resourceRoot(
    `draft-${index}`,
    directory,
    index === 0 ? "主目录" : `附加目录 ${index}`,
    null,
  ));
}

function resourceRoot(
  id: string,
  directory: string,
  detail: string,
  locator: SessionResourceLocator | null,
): SessionResourceRootItem {
  const segments = directory.replace(/\/+$/, "").split("/");
  return {
    id,
    label: segments.at(-1) || directory,
    detail,
    locator,
    path: directory,
  };
}
