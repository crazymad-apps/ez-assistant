import type { ResourceHandle, ResourceTab } from "./ResourceWorkspaceStore";
import type { ResourceViewState } from "./resourceViewState";
import type { TerminalSource } from "../../native-bridge/userTerminal";
import type { SessionResourceLocator } from "../../generated/assistant-protocol";

/** 只保存重建描述，不保存 controller、原生句柄、文件内容或终端命令/输出。 */
export type SavedResourceTab = Readonly<{
  page: { type: "context" | "workspace" }
    | { type: "browser"; url: string; title: string }
    | { type: "terminal"; source: TerminalSource }
    | { type: "resource"; name: string; source: ResourceHandle["source"]; line: number | null };
  view_state?: ResourceViewState;
}>;
export type ResourceWorkspaceSnapshot = Readonly<{
  current_scope_key: string;
  groups: readonly Readonly<{
    scope_key: string;
    active_index: number;
    focused_index: number;
    workspace_location?: SessionResourceLocator;
    tabs: readonly SavedResourceTab[];
  }>[];
}>;

export function localFileUri(segments: readonly string[]): string {
  return `file:///${segments.filter((part) => part !== "/").map(encodeURIComponent).join("/")}`;
}

export function resourceIdentity(group: { id: string; scope_key: string }, source: ResourceHandle["source"]): string {
  switch (source.type) {
    case "session_file": {
      const root = source.locator.root;
      const root_key = root.type === "workspace_additional" ? `${root.type}:${root.directory_index}` : root.type;
      return `${group.id}:${source.session_id}:${root_key}:${source.locator.relative_path}`;
    }
    case "attachment": return `${group.scope_key}:attachment:${source.session_id}:${source.attachment_id}`;
    case "tool_file": {
      const owner = source.owner;
      const key = owner.type === "child_task" ? `${owner.session_id}:child:${owner.child_task_id}` : `${owner.session_id}:main`;
      return `${group.scope_key}:tool:${key}:${source.message_id}:${source.resource_ref_id}`;
    }
    case "local_file": return localFileUri(source.path_segments);
  }
}

export function savedResourcePage(tab: Extract<ResourceTab, { resource: ResourceHandle }>): SavedResourceTab["page"] {
  return { type: "resource", name: tab.resource.display_name, source: tab.resource.source, line: tab.type === "text" ? tab.line : null };
}

/** 偏好文件不是受信任的运行时对象；先检查结构/预算，再交给已有资源 API 重新校验访问。 */
export function parseResourceSnapshot(value: unknown): ResourceWorkspaceSnapshot | null {
  if (value == null) return null;
  if (!record(value) || !scope(value.current_scope_key) || !Array.isArray(value.groups) || value.groups.length > 256) throw invalid();
  let tabs = 0;
  const seen = new Set<string>();
  for (const group of value.groups) {
    if (!record(group) || !scope(group.scope_key) || seen.has(group.scope_key) || !Array.isArray(group.tabs)
      || !Number.isInteger(group.active_index) || !Number.isInteger(group.focused_index)) throw invalid();
    seen.add(group.scope_key);
    tabs += group.tabs.length;
    if (tabs > 2048 || group.tabs.length > 256 || (group.workspace_location && !locator(group.workspace_location))) throw invalid();
    for (const tab of group.tabs) {
      if (!record(tab) || !page(tab.page) || (tab.view_state && !viewState(tab.view_state))) throw invalid();
    }
  }
  return value as ResourceWorkspaceSnapshot;
}
function invalid(): Error { return new Error("右栏快照损坏或超出上限，未能恢复。"); }
function record(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }
function text(value: unknown): value is string { return typeof value === "string" && value.length <= 16384; }
function scope(value: unknown): value is string { return text(value) && /^(session:.+|draft:(unbound|workspace:.+))$/.test(value); }
function locator(value: unknown): boolean {
  if (!record(value) || !record(value.root) || !text(value.relative_path)) return false;
  return ["session_private", "workspace_primary"].includes(String(value.root.type))
    || (value.root.type === "workspace_additional" && Number.isInteger(value.root.directory_index) && Number(value.root.directory_index) >= 0);
}
function source(value: unknown): boolean {
  if (!record(value)) return false;
  if (value.type === "session_file") return text(value.session_id) && locator(value.locator);
  if (value.type === "local_file") return Array.isArray(value.path_segments) && value.path_segments.length > 1
    && value.path_segments[0] === "/" && value.path_segments.slice(1).every((part: unknown) => text(part) && part !== ".." && !part.includes("/"));
  if (value.type === "attachment") return text(value.session_id) && text(value.attachment_id) && Array.isArray(value.siblings)
    && value.siblings.every((item: unknown) => record(item) && text(item.attachment_id) && text(item.original_name));
  if (value.type === "tool_file") return record(value.owner) && text(value.owner.session_id)
    && (value.owner.type === "main_session" || (value.owner.type === "child_task" && text(value.owner.child_task_id)))
    && text(value.message_id) && text(value.resource_ref_id) && Array.isArray(value.siblings)
    && value.siblings.every((item: unknown) => record(item) && text(item.resource_ref_id) && text(item.display_name));
  return false;
}
function page(value: unknown): boolean {
  if (!record(value)) return false;
  if (value.type === "context" || value.type === "workspace") return true;
  if (value.type === "browser") return text(value.url) && text(value.title)
    && (!value.url || /^https?:\/\//.test(value.url));
  if (value.type === "terminal") return record(value.source) && ((value.source.type === "workspace" && text(value.source.workspace_id))
    || (value.source.type === "session" && text(value.source.session_id) && locator(value.source.locator)));
  return value.type === "resource" && text(value.name) && source(value.source) && (value.line === null || Number.isInteger(value.line));
}
function viewState(value: unknown): boolean {
  if (!record(value)) return false;
  if (value.roots && (!Array.isArray(value.roots) || !value.roots.every((root: unknown) => record(root) && text(root.label) && (!root.locator || locator(root.locator))))) return false;
  const tree = value.tree;
  if (tree && (!record(tree) || !Array.isArray(tree.expanded) || !tree.expanded.every(locator)
    || typeof tree.include_hidden !== "boolean" || typeof tree.include_generated !== "boolean" || !Number.isFinite(tree.scroll_top))) return false;
  const preview = value.preview;
  if (preview && (!record(preview) || !Number.isFinite(preview.scroll_top) || !Number.isFinite(preview.scroll_left)
    || typeof preview.word_wrap !== "boolean"
    || (preview.editor != null && (!record(preview.editor) || !Array.isArray(preview.editor.cursorState) || !record(preview.editor.viewState))))) return false;
  if (record(preview) && preview.image !== undefined) {
    const image = preview.image;
    if (!record(image) || !Number.isFinite(image.scale) || Number(image.scale) <= 0
      || !Number.isFinite(image.position_x) || !Number.isFinite(image.position_y)) return false;
  }
  return true;
}
