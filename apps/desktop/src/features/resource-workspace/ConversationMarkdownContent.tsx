import { useCallback, useState } from "react";
import { MarkdownContent } from "../../components/MarkdownContent";
import {
  openLocalResourceInSystem,
  previewLocalResource,
  registerLocalFileUri,
  revealLocalResourceInDirectory,
  type RegisteredLocalResource,
} from "../../native-bridge/nativeResource";
import { useRootStore } from "../../stores/RootStoreContext";
import { createResourceObjectUrl } from "../../native-bridge/resourceObjectUrl";
import { ResourceContextMenu, type ResourceMenuLocation } from "./ResourceContextMenu";

export function ConversationMarkdownContent(props: Readonly<{
  is_streaming?: boolean;
  text: string;
}>) {
  const root_store = useRootStore();
  const session_id = root_store.navigation.selected_session_id;
  const draft_key = root_store.navigation.selected_draft_key;
  const scope_key = session_id ? `session:${session_id}` : draft_key ? `draft:${draft_key}` : null;
  const [menu, setMenu] = useState<Readonly<{
    location: ResourceMenuLocation;
    resource: RegisteredLocalResource;
  }> | null>(null);

  const loadLocalImage = useCallback(async (reference: string) => {
    const registered = await registerLocalFileUri(reference);
    const preview = await previewLocalResource(registered.resource_key);
    if (preview.kind !== "image" || !preview.data_base64) {
      throw new Error("该资源不是可预览图片。");
    }
    return createResourceObjectUrl(preview.data_base64, preview.media_type);
  }, []);

  const openLocalResource = useCallback((reference: string) => {
    if (!scope_key) return;
    void registerLocalFileUri(reference)
      .then((registered) => {
        root_store.resource_workspace.openLocalResource(scope_key, registered);
        if (!root_store.navigation.effective_right_sidebar_open) root_store.toggleRightSidebar();
      })
      .catch((failure: unknown) => {
        root_store.showInteractionError(failure instanceof Error ? failure.message : "无法打开本地资源。");
      });
  }, [root_store, scope_key]);

  const openLocalResourceMenu = useCallback((reference: string, location: ResourceMenuLocation) => {
    if (!scope_key) return;
    void registerLocalFileUri(reference)
      .then((resource) => setMenu({ location, resource }))
      .catch((failure: unknown) => {
        root_store.showInteractionError(failure instanceof Error ? failure.message : "无法打开本地资源菜单。");
      });
  }, [root_store, scope_key]);

  const runResourceAction = useCallback((request: Promise<void>, fallback: string) => {
    void request.catch((failure: unknown) => {
      root_store.showInteractionError(failure instanceof Error ? failure.message : fallback);
    });
  }, [root_store]);

  return (
    <>
      <MarkdownContent
        is_streaming={props.is_streaming}
        load_local_image={scope_key ? loadLocalImage : undefined}
        on_local_resource_context_menu={scope_key ? openLocalResourceMenu : undefined}
        on_local_resource_open={scope_key ? openLocalResource : undefined}
        text={props.text}
      />
      {menu && scope_key && (
        <ResourceContextMenu
          items={[
            {
              label: "在资源栏打开",
              on_select: () => {
                root_store.resource_workspace.openLocalResource(scope_key, menu.resource);
                if (!root_store.navigation.effective_right_sidebar_open) root_store.toggleRightSidebar();
              },
            },
            {
              label: "使用系统应用打开",
              on_select: () => runResourceAction(
                openLocalResourceInSystem(menu.resource.resource_key),
                "无法使用系统应用打开。",
              ),
            },
            {
              label: "在 Finder 中显示",
              on_select: () => runResourceAction(
                revealLocalResourceInDirectory(menu.resource.resource_key),
                "无法在 Finder 中显示。",
              ),
            },
          ]}
          location={menu.location}
          on_close={() => setMenu(null)}
        />
      )}
    </>
  );
}
