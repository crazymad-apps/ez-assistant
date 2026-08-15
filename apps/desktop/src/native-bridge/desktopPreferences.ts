import { invoke, isTauri } from "@tauri-apps/api/core";

export type DesktopPreferences = {
  readonly left_sidebar_open: boolean;
  readonly right_sidebar_open: boolean;
  readonly expanded_workspace_ids: readonly string[] | null;
};

export async function loadDesktopPreferences(): Promise<DesktopPreferences> {
  if (!isTauri()) {
    return {
      left_sidebar_open: true,
      right_sidebar_open: true,
      expanded_workspace_ids: null,
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
