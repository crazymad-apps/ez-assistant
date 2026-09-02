import { invoke, isTauri } from "@tauri-apps/api/core";

export type DesktopPreferences = {
  readonly left_sidebar_open: boolean;
  readonly right_sidebar_open: boolean;
  readonly left_sidebar_width: number;
  readonly right_sidebar_width: number;
  readonly expanded_workspace_ids: readonly string[] | null;
  readonly close_behavior: DesktopCloseBehavior;
};

export type DesktopCloseBehavior = "hide_to_tray" | "quit_desktop";

export async function loadDesktopPreferences(): Promise<DesktopPreferences> {
  if (!isTauri()) {
    return {
      left_sidebar_open: true,
      right_sidebar_open: true,
      left_sidebar_width: 286,
      right_sidebar_width: 326,
      expanded_workspace_ids: null,
      close_behavior: "hide_to_tray",
    };
  }
  return invoke<DesktopPreferences>("load_desktop_preferences");
}

export async function saveDesktopPreferences(preferences: DesktopPreferences): Promise<void> {
  if (!isTauri()) {
    return;
  }
  await invoke("save_desktop_preferences", { preferences });
}
