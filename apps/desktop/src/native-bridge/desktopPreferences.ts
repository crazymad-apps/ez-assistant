import { invoke, isTauri } from "@tauri-apps/api/core";
import type { ApprovalMode, ModelSelection } from "@ez-assistant/protocol";
import { parseResourceSnapshot, type ResourceWorkspaceSnapshot } from "../features/resource-workspace/resourceWorkspaceSnapshot";
import { DEFAULT_THEME_ID, isThemeId, type ThemeId } from "../styles/themePresets";

export type DesktopPreferences = {
  readonly desktop_workspace?: unknown;
  readonly left_sidebar_open: boolean;
  readonly right_sidebar_open: boolean;
  readonly left_sidebar_width: number;
  /** 基准窗口剩余空间中的右栏宽度；实际像素随窗口按比例计算。 */
  readonly right_sidebar_width: number;
  readonly expanded_workspace_ids: readonly string[] | null;
  readonly close_behavior: DesktopCloseBehavior;
  readonly default_approval_mode: ApprovalMode;
  readonly last_model_selection: ModelSelection | null;
  readonly resource_workspace?: ResourceWorkspaceSnapshot | null;
};

export type DesktopCloseBehavior = "hide_to_tray" | "quit_desktop";

const defaults: DesktopPreferences = { left_sidebar_open: true, right_sidebar_open: true, left_sidebar_width: 286, right_sidebar_width: 380, expanded_workspace_ids: null, close_behavior: "hide_to_tray", default_approval_mode: "auto", last_model_selection: null };
const WEB_KEY = "ez-assistant:view";
const WEB_THEME_KEY = "ez-assistant:theme";

export type DesktopThemePreference = {
  readonly theme_id: ThemeId;
  readonly recovered_unknown: boolean;
};

/** Web 保持同步读写，原生复用现有偏好文件；均不读写身份分区的布局快照。 */
export function loadDesktopTheme(): DesktopThemePreference | Promise<DesktopThemePreference> {
  if (isTauri()) return invoke<DesktopThemePreference>("load_desktop_theme");
  const value = localStorage.getItem(WEB_THEME_KEY);
  if (value === null) return { theme_id: DEFAULT_THEME_ID, recovered_unknown: false };
  return {
    theme_id: isThemeId(value) ? value : DEFAULT_THEME_ID,
    recovered_unknown: !isThemeId(value),
  };
}

export function saveDesktopTheme(theme_id: ThemeId): void | Promise<void> {
  if (!isThemeId(theme_id)) throw new Error("不支持的配色方案。");
  if (isTauri()) return invoke<void>("save_desktop_theme", { themeId: theme_id });
  // 布局按整份 JSON 保存，独立标量键避免其他标签的旧布局覆盖主题，不新增存储设施。
  localStorage.setItem(WEB_THEME_KEY, theme_id);
}

export async function applyDesktopTheme(theme_id: ThemeId): Promise<void> {
  if (isTauri()) await invoke<void>("apply_desktop_theme", { themeId: theme_id });
}

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
    right_sidebar_width: typeof data.right_sidebar_width === "number" && Number.isFinite(data.right_sidebar_width) ? Math.max(1, data.right_sidebar_width) : 380,
    expanded_workspace_ids: null,
    resource_workspace: null,
  };
}
export async function saveDesktopPreferences(preferences: DesktopPreferences, namespace?: string): Promise<void> {
  if (isTauri()) { await invoke("save_desktop_preferences", {preferences, namespace: namespace ?? null}); return; }
  // 同源账号共享 Cookie，只持久化无用户数据的布局；资源、会话 ID 不写浏览器缓存。
  // 普通 Web 不消费或保存 Desktop 设备默认，使用 Auto 并跟随 Runtime 全局模型默认。
  const text = JSON.stringify({
    left_sidebar_open: preferences.left_sidebar_open,
    right_sidebar_open: preferences.right_sidebar_open,
    left_sidebar_width: preferences.left_sidebar_width,
    right_sidebar_width: preferences.right_sidebar_width,
    close_behavior: preferences.close_behavior,
  });
  if (text.length > 4 * 1024 * 1024) throw new Error("视图记录过大，本次未保存。");
  localStorage.setItem(WEB_KEY, text);
}
