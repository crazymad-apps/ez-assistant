import { useCallback, useState } from "react";
import { MarkdownContent } from "../../components/MarkdownContent";
import { openLocalResourceInSystem, registerLocalFileUri, revealLocalResourceInDirectory } from "../../native-bridge/nativeResource";
import { hostFilePath, hostFileUri } from "../../runtime-client/hostFilePath";
import { useRootStore } from "../../stores/RootStoreContext";
import { createResourceObjectUrl } from "../../native-bridge/resourceObjectUrl";
import { ResourceContextMenu, type ResourceMenuLocation } from "./ResourceContextMenu";

export function ConversationMarkdownContent(props: Readonly<{ is_streaming?: boolean; text: string }>) {
  const root = useRootStore();
  const session = root.navigation.selected_session_id;
  const draft = root.navigation.selected_draft_key;
  const scope = session ? `session:${session}` : draft ? `draft:${draft}` : null;
  const directory = session ? root.projection.session_views.get(session)?.workspace?.primary_directory : undefined;
  const base = directory ? `${directory.replace(/\/$/, "")}/` : undefined;
  const [menu, setMenu] = useState<{location: ResourceMenuLocation; path: string} | null>(null);
  const showError = useCallback((error: unknown) => root.showInteractionError(error instanceof Error ? error.message : "无法读取 Host 文件。"), [root]);
  const loadImage = useCallback(async (reference: string) => {
    const preview = await root.files.previewHostFile(hostFilePath(reference, base));
    if (preview.kind !== "image" || !preview.data_base64) throw new Error("该资源不是可预览图片。");
    return createResourceObjectUrl(preview.data_base64, preview.media_type);
  }, [root, base]);
  const open = useCallback((reference: string) => {
    if (!scope) return;
    try {
      root.resource_workspace.openHostFile(scope, hostFilePath(reference, base));
      if (!root.navigation.effective_right_sidebar_open) root.toggleRightSidebar();
    } catch (error) { showError(error); }
  }, [root, scope, showError, base]);
  return <>
    <MarkdownContent allow_relative_local_resources={!!base} is_streaming={props.is_streaming} text={props.text} load_local_image={scope ? loadImage : undefined}
      on_local_resource_open={scope ? open : undefined}
      on_local_resource_context_menu={scope ? (reference, location) => { try { setMenu({path:hostFilePath(reference, base),location}); } catch (error) { showError(error); } } : undefined}/>
    {menu && scope && <ResourceContextMenu location={menu.location} on_close={() => setMenu(null)} items={[
      {label:"在资源栏打开",on_select:()=>open(hostFileUri(menu.path))},
      {label:"下载文件",on_select:()=>{void root.files.downloadHostFile(menu.path).catch(showError);}},
      ...(root.files.native_host ? [
        {label:"使用系统应用打开",on_select:()=>{void registerLocalFileUri(hostFileUri(menu.path)).then((r)=>openLocalResourceInSystem(r.resource_key)).catch(showError);}},
        {label:"在 Finder 中显示",on_select:()=>{void registerLocalFileUri(hostFileUri(menu.path)).then((r)=>revealLocalResourceInDirectory(r.resource_key)).catch(showError);}},
      ] : []),
    ]}/>}
  </>;
}
