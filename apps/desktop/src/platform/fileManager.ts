/** 原生文件定位发生在客户端；平台值由 AppShell 的 desktop_platform 查询写入。 */
export function revealFileLabel(): string {
  switch (document.documentElement.dataset.platform) {
    case "windows": return "在资源管理器中显示";
    case "macos": return "在 Finder 中显示";
    default: return "在文件管理器中显示";
  }
}
