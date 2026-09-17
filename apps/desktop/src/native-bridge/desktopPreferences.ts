import { invoke, isTauri } from "@tauri-apps/api/core";
import type { ApprovalMode, ModelSelection } from "@ez-assistant/protocol";
import { parseResourceSnapshot, type ResourceWorkspaceSnapshot } from "../features/resource-workspace/resourceWorkspaceSnapshot";

export type DesktopPreferences = {
  readonly left_sidebar_open: boolean;
  readonly right_sidebar_open: boolean;
  readonly left_sidebar_width: number;
  readonly right_sidebar_width: number;
  readonly expanded_workspace_ids: readonly string[] | null;
  readonly close_behavior: DesktopCloseBehavior;
  readonly default_approval_mode: ApprovalMode;
  readonly last_model_selection: ModelSelection | null;
  readonly resource_workspace?: ResourceWorkspaceSnapshot | null;
};

export type DesktopCloseBehavior = "hide_to_tray" | "quit_desktop";

const defaults: DesktopPreferences = { left_sidebar_open: true, right_sidebar_open: true, left_sidebar_width: 286, right_sidebar_width: 380, expanded_workspace_ids: null, close_behavior: "hide_to_tray", default_approval_mode: "ask", last_model_selection: null };
const WEB_KEY = "ez-assistant:view";

export function viewingSnapshot(value: ResourceWorkspaceSnapshot | null | undefined, native: boolean, local = native): ResourceWorkspaceSnapshot | null {
  const snapshot = parseResourceSnapshot(value);
  if (!snapshot) return null;
  return {...snapshot, groups: snapshot.groups.map((group) => {
    const allowed = group.tabs.map((tab, index) => ({tab, index})).filter(({tab}) => (native || (tab.page.type !== "browser" && tab.page.type !== "terminal")) && (local || !(tab.page.type === "resource" && tab.page.source.type === "local_file")));
    return {...group, tabs: allowed.map(({tab}) => tab), active_index: Math.max(0, allowed.findIndex(({index}) => index === group.active_index)), focused_index: Math.max(0, allowed.findIndex(({index}) => index === group.focused_index))};
  })};
}
export async function loadDesktopPreferences(namespace?: string): Promise<DesktopPreferences> {
  if (isTauri()) return invoke<DesktopPreferences>("load_desktop_preferences", {namespace: namespace ?? null});
  const value = localStorage.getItem(WEB_KEY);
  if (!value) return defaults;
  if (value.length > 4 * 1024 * 1024) throw new Error("视图记录过大，已使用默认布局。");
  const data = JSON.parse(value) as Partial<DesktopPreferences>;
  return {
    ...defaults,
    left_sidebar_open: typeof data.left_sidebar_open === "boolean" ? data.left_sidebar_open : true,
    right_sidebar_open: typeof data.right_sidebar_open === "boolean" ? data.right_sidebar_open : true,
    left_sidebar_width: typeof data.left_sidebar_width === "number" && Number.isFinite(data.left_sidebar_width) ? Math.min(420, Math.max(220, data.left_sidebar_width)) : 286,
    right_sidebar_width: typeof data.right_sidebar_width === "number" && Number.isFinite(data.right_sidebar_width) ? Math.max(320, data.right_sidebar_width) : 380,
    expanded_workspace_ids: Array.isArray(data.expanded_workspace_ids) ? data.expanded_workspace_ids.filter((id): id is string => typeof id === "string" && id.length <= 256).slice(0,256) : null,
    resource_workspace: viewingSnapshot(data.resource_workspace, false),
  };
}
export async function saveDesktopPreferences(preferences: DesktopPreferences, namespace?: string): Promise<void> {
  if (isTauri()) { await invoke("save_desktop_preferences", {preferences, namespace: namespace ?? null}); return; }
  // 同步写入 localStorage，pagehide 时无需等待一个异步任务；同源浏览器存储天然按 Host 隔离。
  // 普通 Web 不消费或保存 Desktop 设备默认，继续固定 Ask 并跟随 Runtime 全局模型默认。
  const text = JSON.stringify({
    left_sidebar_open: preferences.left_sidebar_open,
    right_sidebar_open: preferences.right_sidebar_open,
    left_sidebar_width: preferences.left_sidebar_width,
    right_sidebar_width: preferences.right_sidebar_width,
    expanded_workspace_ids: preferences.expanded_workspace_ids,
    close_behavior: preferences.close_behavior,
    resource_workspace: viewingSnapshot(preferences.resource_workspace, false),
  });
  if (text.length > 4 * 1024 * 1024) throw new Error("视图记录过大，本次未保存。");
  localStorage.setItem(WEB_KEY, text);
}
