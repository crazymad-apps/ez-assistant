import type { IconName } from "../components/Icon";

/** 设备选择入口按 Host 上报的平台展示，未知平台保留通用电脑图标。 */
export function deviceIcon(platform: string | undefined): IconName {
  switch (platform) {
    case "windows": return "windows";
    case "macos": return "macos";
    case "linux": return "linux";
    default: return "desktop";
  }
}
