import { isTauri } from "@tauri-apps/api/core";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import { IconMenuItem, Menu, NativeIcon } from "@tauri-apps/api/menu";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";

export const supportsNativeResourceMenu = isTauri;

export async function createResourceMenu(on_workspace: () => void, on_browser: () => void, on_terminal?: () => void) {
  const items: IconMenuItem[] = [];
  let menu: Menu | undefined;
  try {
    items.push(await IconMenuItem.new({ text: "工作空间", icon: NativeIcon.Folder, action: on_workspace }));
    items.push(await IconMenuItem.new({ text: "浏览器", icon: NativeIcon.Network, action: on_browser }));
    items.push(await IconMenuItem.new({ text: "终端", icon: NativeIcon.Computer, enabled: false, action: on_terminal }));
    menu = await Menu.new({ items });
  } catch (failure) {
    await Promise.allSettled(items.map((item) => item.close()));
    throw failure;
  }
  const native_menu = menu;
  return {
    async popup(workspace_available: boolean, anchor: Readonly<{ x: number; y: number }>, terminal_available = false): Promise<void> {
      await items[0]!.setEnabled(workspace_available);
      await items[2]!.setEnabled(terminal_available);
      const window = getCurrentWindow();
      const view = getCurrentWebview();
      const [size, position, scale] = await Promise.all([view.size(), view.position(), window.scaleFactor()]);
      const zoom = size.width / scale / globalThis.innerWidth;
      const top = Math.max(0, size.height / scale - globalThis.innerHeight * zoom);
      await native_menu.popup(new LogicalPosition(position.x / scale + anchor.x * zoom, position.y / scale + top + anchor.y * zoom), window);
    },
    async dispose(): Promise<void> {
      const results = await Promise.allSettled([native_menu.close(), ...items.map((item) => item.close())]);
      const failure = results.find((result) => result.status === "rejected");
      if (failure?.status === "rejected") throw failure.reason;
    },
  };
}
