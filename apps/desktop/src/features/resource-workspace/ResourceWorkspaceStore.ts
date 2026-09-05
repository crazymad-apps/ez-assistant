import { action, computed, makeObservable, observable, runInAction } from "mobx";
import { TerminalController } from "./TerminalController";
import type { TerminalSource } from "../../native-bridge/userTerminal";
import { BrowserController } from "./BrowserController";
import type { ResourceViewState } from "./resourceViewState";
import type {
  AttachmentSummary,
  ConversationOwner,
  MessageId,
  SessionId,
  SessionResourceLocator,
  ToolFileReference,
} from "../../generated/assistant-protocol";
import { registerLocalFileUri, type RegisteredLocalResource } from "../../native-bridge/nativeResource";
import { localFileUri, resourceIdentity, savedResourcePage, type ResourceWorkspaceSnapshot, type SavedResourceTab } from "./resourceWorkspaceSnapshot";

export type ResourceScopeKey = string;

export type ResourceHandle = Readonly<{
  resource_key: string;
  display_name: string;
  scope_key: ResourceScopeKey;
  source: Readonly<
    | {
      type: "session_file";
      session_id: SessionId;
      locator: SessionResourceLocator;
    }
    | {
      type: "local_file";
      path_segments: readonly string[];
    }
    | {
      type: "attachment";
      session_id: SessionId;
      attachment_id: string;
      siblings: readonly AttachmentSummary[];
    }
    | {
      type: "tool_file";
      owner: ConversationOwner;
      message_id: MessageId;
      resource_ref_id: string;
      siblings: readonly ToolFileReference[];
    }
  >;
}>;

export type ResourceTab =
  | Readonly<{ type: "context" }>
  | Readonly<{ type: "workspace"; scopeKey: ResourceScopeKey }>
  | Readonly<{ type: "text"; resource: ResourceHandle; line: number | null }>
  | Readonly<{ type: "markdown"; resource: ResourceHandle }>
  | Readonly<{ type: "image"; resource: ResourceHandle }>
  | Readonly<{ type: "pdf"; resource: ResourceHandle }>
  | Readonly<{ type: "browser"; browserId: string }>
  | Readonly<{ type: "terminal"; terminalId: string }>;

const CONTEXT_TAB: ResourceTab = { type: "context" };
export const CONTEXT_TAB_KEY = "context";

/** 一个 Session/草稿的轻量标签索引；草稿物化只转移此 owner，不复制浏览器或 PTY。 */
let next_group_id = 1;

export class ResourceTabGroup {
  readonly id: string;
  scope_key: ResourceScopeKey;
  readonly tabs = observable.array<ResourceTab>([CONTEXT_TAB], { deep: false });
  active_tab_key = CONTEXT_TAB_KEY;
  focused_tab_key = CONTEXT_TAB_KEY;
  closing = false;

  constructor(scope_key: ResourceScopeKey) {
    this.id = `group-${next_group_id++}`;
    this.scope_key = scope_key;
    makeObservable(this, { scope_key: observable, active_tab_key: observable, focused_tab_key: observable, closing: observable });
  }
}

export type CachedResourcePage = Readonly<{ key: string; group: ResourceTabGroup; tab: ResourceTab }>;
const PAGE_CACHE_LIMIT = 20;

export class ResourceWorkspaceStore {
  readonly groups = observable.map<ResourceScopeKey, ResourceTabGroup>({}, { deep: false });
  current_scope_key: ResourceScopeKey = "draft:unbound";
  readonly cached_pages = observable.map<string, CachedResourcePage>({}, { deep: false });
  readonly view_states = new Map<string, ResourceViewState>();
  shutting_down = false;
  readonly workspace_locations = observable.map<ResourceScopeKey, SessionResourceLocator>();
  readonly browsers = observable.map<string, BrowserController>({}, { deep: false });
  readonly terminals = observable.map<string, TerminalController>({}, { deep: false });
  #restoring = false;
  #generation = 0;
  #next_terminal_id = 1;
  browser_error: string | null = null;
  #next_browser_id = 1;
  #next_resource_key = 1;
  readonly #resource_keys = new Map<string, string>();

  constructor() {
    makeObservable(this, {
      current_scope_key: observable, shutting_down: observable,
      tabs: computed, active_group: computed, mounted_pages: computed,
      active_tab_key: computed, focused_tab_key: computed,
      workspace_locations: observable,
      browser_error: observable,
      openBrowser: action,
      openTerminal: action,
      active_tab: computed,
      openTab: action,
      activateTab: action,
      focusTab: action,
      moveFocus: action,
      closeTab: action,
      openWorkspace: action,
      openSessionResource: action,
      openLocalResource: action,
      openAttachment: action,
      openToolResource: action,
      setWorkspaceLocation: action,
      selectScope: action, transferDraft: action, releaseScope: action,
      dispose: action,
    });
    runInAction(() => this.groups.set(this.current_scope_key, new ResourceTabGroup(this.current_scope_key)));
  }

  get active_group(): ResourceTabGroup { return this.groups.get(this.current_scope_key)!; }
  get tabs() { return this.active_group.tabs; }
  get active_tab_key(): string { return this.active_group.active_tab_key; }
  set active_tab_key(value: string) { this.active_group.active_tab_key = value; }
  get focused_tab_key(): string { return this.active_group.focused_tab_key; }
  set focused_tab_key(value: string) { this.active_group.focused_tab_key = value; }

  get mounted_pages(): CachedResourcePage[] {
    const pages = [...this.cached_pages.values()];
    for (const group of this.groups.values()) {
      for (const tab of group.tabs) {
        if (tab.type === "terminal") pages.push({ key: this.pageKey(group, tab), group, tab });
      }
    }
    return pages;
  }

  pageKey(group: ResourceTabGroup, tab: ResourceTab): string {
    return `${group.id}/${tab.type === "workspace" ? "workspace" : resourceTabKey(tab)}`;
  }

  pageState(key: string): ResourceViewState {
    let state = this.view_states.get(key);
    if (!state) { state = {}; this.view_states.set(key, state); }
    return state;
  }

  openTerminal(source: TerminalSource, owner = this.active_group, deferred = false): void {
    if (this.shutting_down || owner.closing || this.groups.get(owner.scope_key) !== owner) return;
    const terminalId = `terminal-${this.#next_terminal_id++}`;
    const controller = new TerminalController(source, () => {
      // Ctrl+D 退出也会继承上条命令的非零状态，不能据退出码决定是否关页。
      // 复用显式关闭的回收顺序，接住创建早于响应的退出事件；后台退出不抢占当前标签。
      void this.closeTerminalTab(terminalId).catch((failure: unknown) => controller.reportError(failure));
    }, deferred);
    this.terminals.set(terminalId, controller);
    this.openTab({ type: "terminal", terminalId }, owner);
  }

  async closeTerminalTab(terminalId: string): Promise<string> {
    await this.terminals.get(terminalId)?.close();
    return this.closeTab(`terminal:${terminalId}`);
  }

  openBrowser(url?: string, owner = this.active_group): void {
    if (this.shutting_down || owner.closing || this.groups.get(owner.scope_key) !== owner) return;
    const browserId = `browser-${this.#next_browser_id++}`;
    const controller = new BrowserController(
      (address) => this.openBrowser(address, owner),
      (message) => runInAction(() => { this.browser_error = message; }),
    );
    this.browsers.set(browserId, controller);
    this.openTab({ type: "browser", browserId }, owner);
    if (owner !== this.active_group) controller.suspend();
    if (url) controller.navigate(url);
  }

  openWorkspace(scopeKey: ResourceScopeKey, locator?: SessionResourceLocator): void {
    const group = this.groups.get(scopeKey);
    if (!group || group.closing || this.shutting_down) return;
    if (locator) this.workspace_locations.set(scopeKey, locator);
    this.openTab({ type: "workspace", scopeKey }, group);
  }

  openSessionResource(
    scope_key: ResourceScopeKey,
    session_id: SessionId,
    locator: SessionResourceLocator,
    display_name: string,
    line: number | null = null,
  ): void {
    const group = this.groups.get(scope_key);
    if (!group) return;
    const identity = `${group.id}:${session_id}:${locatorIdentity(locator)}`;
    let resource_key = this.#resource_keys.get(identity);
    if (!resource_key) {
      resource_key = `resource-${this.#next_resource_key}`;
      this.#next_resource_key += 1;
      this.#resource_keys.set(identity, resource_key);
    }
    const resource: ResourceHandle = {
      resource_key,
      display_name,
      scope_key,
      source: { type: "session_file", session_id, locator },
    };
    const type = resourceType(display_name);
    this.openTab(type === "text"
      ? { type, resource, line }
      : { type, resource }, group);
  }

  openLocalResource(
    scope_key: ResourceScopeKey,
    registered: RegisteredLocalResource,
    line: number | null = null,
  ): void {
    const group = this.groups.get(scope_key);
    if (!group) return;
    const resource: ResourceHandle = {
      resource_key: registered.resource_key,
      display_name: registered.display_name,
      scope_key,
      source: { type: "local_file", path_segments: registered.path_segments },
    };
    const type = resourceType(registered.display_name);
    this.openTab(type === "text"
      ? { type, resource, line }
      : { type, resource }, group);
  }

  openAttachment(
    scope_key: ResourceScopeKey,
    attachment: AttachmentSummary,
    siblings: readonly AttachmentSummary[] = [attachment],
  ): void {
    const identity = `${scope_key}:attachment:${attachment.session_id}:${attachment.attachment_id}`;
    const resource_key = this.#stableResourceKey(identity);
    this.#openResource({
      resource_key,
      display_name: attachment.original_name,
      scope_key,
      source: {
        type: "attachment",
        session_id: attachment.session_id,
        attachment_id: attachment.attachment_id,
        siblings,
      },
    });
  }

  openToolResource(
    scope_key: ResourceScopeKey,
    owner: ConversationOwner,
    message_id: MessageId,
    file: ToolFileReference,
    siblings: readonly ToolFileReference[] = [file],
  ): void {
    const owner_key = owner.type === "child_task"
      ? `${owner.session_id}:child:${owner.child_task_id}`
      : `${owner.session_id}:main`;
    const resource_key = this.#stableResourceKey(`${scope_key}:tool:${owner_key}:${message_id}:${file.resource_ref_id}`);
    this.#openResource({
      resource_key,
      display_name: file.display_name,
      scope_key,
      source: {
        type: "tool_file",
        owner,
        message_id,
        resource_ref_id: file.resource_ref_id,
        siblings,
      },
    });
  }

  setWorkspaceLocation(scopeKey: ResourceScopeKey, locator: SessionResourceLocator): void {
    this.workspace_locations.set(scopeKey, locator);
  }

  selectScope(scopeKey: ResourceScopeKey): void {
    if (!this.groups.has(scopeKey)) this.groups.set(scopeKey, new ResourceTabGroup(scopeKey));
    this.current_scope_key = scopeKey;
    this.#touchActivePage();
  }

  transferDraft(draftKey: string, sessionId: string): void {
    const oldKey = `draft:${draftKey}`;
    const nextKey = `session:${sessionId}`;
    const group = this.groups.get(oldKey);
    if (!group || this.groups.has(nextKey)) return;
    this.groups.delete(oldKey);
    group.scope_key = nextKey;
    this.groups.set(nextKey, group);
    const oldWorkspaceKey = `workspace:${oldKey}`;
    group.tabs.replace(group.tabs.map((tab) => {
      if (tab.type === "workspace") return { ...tab, scopeKey: nextKey };
      if ("resource" in tab) return { ...tab, resource: { ...tab.resource, scope_key: nextKey } };
      return tab;
    }));
    if (group.active_tab_key === oldWorkspaceKey) group.active_tab_key = `workspace:${nextKey}`;
    if (group.focused_tab_key === oldWorkspaceKey) group.focused_tab_key = `workspace:${nextKey}`;
    const location = this.workspace_locations.get(oldKey);
    if (location) this.workspace_locations.set(nextKey, location);
    this.workspace_locations.delete(oldKey);
    for (const tab of group.tabs) {
      const key = this.pageKey(group, tab);
      if (this.cached_pages.has(key)) this.cached_pages.set(key, { key, group, tab });
    }
    if (this.current_scope_key === oldKey) this.current_scope_key = nextKey;
  }

  captureSnapshot(): ResourceWorkspaceSnapshot {
    const snapshot: ResourceWorkspaceSnapshot = {
      current_scope_key: this.current_scope_key,
      groups: [...this.groups.values()].filter((group) => group.tabs.length > 1 || group.scope_key === this.current_scope_key).map((group) => ({
        scope_key: group.scope_key,
        active_index: group.tabs.findIndex((tab) => resourceTabKey(tab) === group.active_tab_key),
        focused_index: group.tabs.findIndex((tab) => resourceTabKey(tab) === group.focused_tab_key),
        workspace_location: this.workspace_locations.get(group.scope_key),
        tabs: group.tabs.map((tab): SavedResourceTab => {
          let page: SavedResourceTab["page"];
          if ("resource" in tab) page = savedResourcePage(tab);
          else if (tab.type === "browser") {
            const browser = this.browsers.get(tab.browserId)!;
            page = { type: "browser", url: browser.url, title: browser.title };
          } else if (tab.type === "terminal") page = { type: "terminal", source: this.terminals.get(tab.terminalId)!.source };
          else page = { type: tab.type };
          return { page, view_state: this.view_states.get(this.pageKey(group, tab)) };
        }),
      })),
    };
    // 保存任务排队期间视图仍会变化；冻结本次描述，不能让退出清理改写它。
    return JSON.parse(JSON.stringify(snapshot)) as ResourceWorkspaceSnapshot;
  }

  async restoreSnapshot(snapshot: ResourceWorkspaceSnapshot): Promise<void> {
    const generation = this.#generation;
    const local = new Map<SavedResourceTab, RegisteredLocalResource>();
    const failed = new Set<SavedResourceTab>();
    for (const group of snapshot.groups) for (const tab of group.tabs) {
      if (tab.page.type !== "resource" || tab.page.source.type !== "local_file") continue;
      try { local.set(tab, await registerLocalFileUri(localFileUri(tab.page.source.path_segments))); }
      catch { failed.add(tab); }
    }
    if (generation !== this.#generation) return;
    runInAction(() => {
      this.#restoring = true;
      try {
        for (const saved of snapshot.groups) {
          let group = this.groups.get(saved.scope_key);
          // 启动期间用户已打开资源时，以当前操作为准，不覆盖它。
          if (group && group.tabs.length > 1) continue;
          if (!group) { group = new ResourceTabGroup(saved.scope_key); this.groups.set(saved.scope_key, group); }
          if (saved.workspace_location) this.workspace_locations.set(saved.scope_key, saved.workspace_location);
          const keys: string[] = [];
          for (const tab of saved.tabs) {
            if (failed.has(tab)) { keys.push(CONTEXT_TAB_KEY); continue; }
            const page = tab.page;
            switch (page.type) {
              case "context": this.openTab(CONTEXT_TAB, group); break;
              case "workspace": this.openWorkspace(group.scope_key); break;
              case "terminal": this.openTerminal(page.source, group, true); break;
              case "browser": {
                this.openBrowser(undefined, group);
                const opened = group.tabs.at(-1)!;
                if (opened.type === "browser") {
                  const browser = this.browsers.get(opened.browserId)!;
                  browser.suspend(); browser.url = page.url; browser.title = page.title;
                }
                break;
              }
              case "resource": {
                const registered = local.get(tab);
                if (registered) this.openLocalResource(group.scope_key, registered, page.line);
                else this.#openResource({ resource_key: this.#stableResourceKey(resourceIdentity(group, page.source)),
                  display_name: page.name, scope_key: group.scope_key, source: page.source }, page.line);
                break;
              }
            }
            const opened = group.tabs.find((item) => resourceTabKey(item) === group.active_tab_key)!;
            keys.push(group.active_tab_key);
            if (tab.view_state) {
              // 恢复后仍认作已消费的面包屑定位，不能重新展开用户后来收起的目录。
              if (tab.view_state.tree?.focus_locator && saved.workspace_location
                && JSON.stringify(tab.view_state.tree.focus_locator) === JSON.stringify(saved.workspace_location)) {
                tab.view_state.tree.focus_locator = this.workspace_locations.get(saved.scope_key);
              }
              this.view_states.set(this.pageKey(group, opened), tab.view_state);
            }
          }
          group.active_tab_key = keys[saved.active_index] ?? CONTEXT_TAB_KEY;
          group.focused_tab_key = keys[saved.focused_index] ?? group.active_tab_key;
        }
        if (failed.size) this.browser_error = `${failed.size} 个本地文件已不存在或无法访问，未恢复这些标签。`;
      } finally { this.#restoring = false; }
      this.#touchActivePage();
    });
  }

  runningTerminalCount(scopeKey?: string): number {
    return [...this.groups.values()].filter((group) => !scopeKey || group.scope_key === scopeKey)
      .flatMap((group) => group.tabs).filter((tab) => tab.type === "terminal"
        && this.terminals.get(tab.terminalId)?.needs_close_confirmation).length;
  }

  async closeScopeTerminals(scopeKey: string): Promise<void> {
    const group = this.groups.get(scopeKey);
    if (!group) return;
    runInAction(() => { group.closing = true; });
    try {
      await Promise.all(group.tabs.filter((tab) => tab.type === "terminal").map((tab) => this.closeTerminalTab(tab.terminalId)));
    } catch (failure) { this.resumeScope(scopeKey); throw failure; }
  }

  resumeScope(scopeKey: string): void {
    runInAction(() => { const group = this.groups.get(scopeKey); if (group) group.closing = false; });
  }

  resumeCreation(): void { runInAction(() => { this.shutting_down = false; }); }

  releaseScope(scopeKey: string): void {
    const group = this.groups.get(scopeKey);
    if (!group) return;
    for (const key of group.tabs.map(resourceTabKey)) this.closeTab(key, group);
    if (group.tabs.some((tab) => tab.type === "terminal")) return;
    this.groups.delete(scopeKey);
    this.workspace_locations.delete(scopeKey);
    if (this.current_scope_key === scopeKey) this.selectScope("draft:unbound");
  }

  async shutdownTerminals(): Promise<void> {
    runInAction(() => { this.shutting_down = true; });
    try { await Promise.all([...this.terminals.keys()].map((id) => this.closeTerminalTab(id))); }
    catch (failure) { runInAction(() => { this.shutting_down = false; }); throw failure; }
  }

  get active_tab(): ResourceTab {
    return this.tabs.find((tab) => resourceTabKey(tab) === this.active_tab_key) ?? CONTEXT_TAB;
  }

  openTab(tab: ResourceTab, group: ResourceTabGroup = this.active_group): void {
    if (!group || group.closing || this.shutting_down || this.groups.get(group.scope_key) !== group) return;
    const key = resourceTabKey(tab);
    const existing_index = group.tabs.findIndex((candidate) => resourceTabKey(candidate) === key);
    if (existing_index >= 0) {
      group.tabs[existing_index] = tab;
    } else if (tab.type !== "context") {
      group.tabs.push(tab);
    }
    this.pageState(this.pageKey(group, tab));
    group.active_tab_key = key;
    group.focused_tab_key = key;
    if (group === this.active_group) this.#touchActivePage();
  }

  activateTab(key: string): void {
    if (!this.#hasTab(key)) return;
    this.active_tab_key = key;
    this.focused_tab_key = key;
    this.#touchActivePage();
  }

  focusTab(key: string): void {
    if (this.#hasTab(key)) this.focused_tab_key = key;
  }

  moveFocus(direction: "previous" | "next" | "first" | "last"): string {
    const current_index = Math.max(0, this.tabs.findIndex(
      (tab) => resourceTabKey(tab) === this.focused_tab_key,
    ));
    let next_index = current_index;
    if (direction === "first") next_index = 0;
    if (direction === "last") next_index = this.tabs.length - 1;
    if (direction === "previous") next_index = (current_index - 1 + this.tabs.length) % this.tabs.length;
    if (direction === "next") next_index = (current_index + 1) % this.tabs.length;
    const next_key = resourceTabKey(this.tabs[next_index] ?? CONTEXT_TAB);
    this.focused_tab_key = next_key;
    return next_key;
  }

  closeTab(key: string, owner?: ResourceTabGroup): string {
    const group = owner ?? (this.#hasTab(key) ? this.active_group : undefined) ?? [...this.groups.values()].find((item) => item.tabs.some((tab) => resourceTabKey(tab) === key));
    if (!group) return this.focused_tab_key;
    if (key === CONTEXT_TAB_KEY) return this.focused_tab_key;
    const index = group.tabs.findIndex((tab) => resourceTabKey(tab) === key);
    if (index < 0) return this.focused_tab_key;
    const fallback_key = resourceTabKey(group.tabs[Math.max(0, index - 1)] ?? CONTEXT_TAB);
    const tab = group.tabs[index];
    if (tab?.type === "browser") {
      this.browsers.get(tab.browserId)?.dispose();
      this.browsers.delete(tab.browserId);
    }
    if (tab?.type === "terminal") {
      const terminal = this.terminals.get(tab.terminalId);
      if (terminal && terminal.status !== "closed") return this.focused_tab_key;
      this.terminals.delete(tab.terminalId);
    }
    if (tab) {
      const pageKey = this.pageKey(group, tab);
      this.cached_pages.delete(pageKey);
      this.view_states.delete(pageKey);
    }
    group.tabs.splice(index, 1);
    if (group.active_tab_key === key) group.active_tab_key = fallback_key;
    if (group.focused_tab_key === key) group.focused_tab_key = fallback_key;
    this.#touchActivePage();
    return this.focused_tab_key;
  }

  dispose(): void {
    this.#generation += 1;
    for (const browser of this.browsers.values()) browser.dispose();
    this.browsers.clear();
    for (const terminal of this.terminals.values()) {
      void terminal.close().catch((failure: unknown) => runInAction(() => { this.browser_error = `终端清理失败：${String(failure)}`; }));
    }
    this.terminals.clear();
    this.cached_pages.clear();
    this.view_states.clear();
    this.groups.clear();
    this.groups.set(this.current_scope_key, new ResourceTabGroup(this.current_scope_key));
    this.workspace_locations.clear();
    this.#resource_keys.clear();
    this.#next_resource_key = 1;
    this.active_tab_key = CONTEXT_TAB_KEY;
    this.focused_tab_key = CONTEXT_TAB_KEY;
  }

  #touchActivePage(): void {
    if (this.#restoring) return;
    const tab = this.active_tab;
    if (tab.type === "context" || tab.type === "terminal") return;
    const group = this.active_group;
    const key = this.pageKey(group, tab);
    this.cached_pages.delete(key);
    // 先释放旧页，再挂载当前页；索引与查看状态保留，后台响应不能把页面重新加入预算。
    while (this.cached_pages.size >= PAGE_CACHE_LIMIT) {
      const oldest = this.cached_pages.values().next().value!;
      this.cached_pages.delete(oldest.key);
      if (oldest.tab.type === "browser") this.browsers.get(oldest.tab.browserId)?.suspend();
    }
    this.cached_pages.set(key, { key, group, tab });
    if (tab.type === "browser") this.browsers.get(tab.browserId)?.resume();
  }

  #hasTab(key: string): boolean {
    return this.tabs.some((tab) => resourceTabKey(tab) === key);
  }

  #stableResourceKey(identity: string): string {
    const existing = this.#resource_keys.get(identity);
    if (existing) return existing;
    const resource_key = `resource-${this.#next_resource_key}`;
    this.#next_resource_key += 1;
    this.#resource_keys.set(identity, resource_key);
    return resource_key;
  }

  #openResource(resource: ResourceHandle, line: number | null = null): void {
    const group = this.groups.get(resource.scope_key);
    if (!group) return;
    const type = resourceType(resource.display_name);
    this.openTab(type === "text" ? { type, resource, line } : { type, resource }, group);
  }
}

export function resourceTabKey(tab: ResourceTab): string {
  switch (tab.type) {
    case "context": return CONTEXT_TAB_KEY;
    case "workspace": return `workspace:${tab.scopeKey}`;
    case "text":
    case "markdown":
    case "image":
    case "pdf": return `resource:${tab.resource.resource_key}`;
    case "browser": return `browser:${tab.browserId}`;
    case "terminal": return `terminal:${tab.terminalId}`;
  }
}

function locatorIdentity(locator: SessionResourceLocator): string {
  const root = locator.root.type === "workspace_additional"
    ? `${locator.root.type}:${locator.root.directory_index}`
    : locator.root.type;
  return `${root}:${locator.relative_path}`;
}

function resourceType(display_name: string): "text" | "markdown" | "image" | "pdf" {
  const extension = display_name.toLocaleLowerCase().split(".").at(-1) ?? "";
  if (["md", "markdown", "mdown", "mkd"].includes(extension)) return "markdown";
  if (["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif"].includes(extension)) return "image";
  if (extension === "pdf") return "pdf";
  return "text";
}

export function isPreviewableResource(display_name: string, media_type?: string | null): boolean {
  if (media_type?.startsWith("image/") || media_type?.startsWith("text/")) return true;
  if (media_type?.startsWith("application/pdf")) return true;
  if (media_type && ["application/json", "application/xml", "application/yaml", "application/toml"].includes(media_type)) return true;
  const extension = display_name.toLocaleLowerCase().split(".").at(-1) ?? "";
  return [
    "md", "markdown", "mdown", "mkd", "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "avif", "pdf",
    "txt", "log", "json", "jsonl", "xml", "yaml", "yml", "toml", "ini", "conf", "csv", "tsv",
    "js", "jsx", "ts", "tsx", "css", "scss", "less", "html", "htm", "svg", "rs", "swift", "py",
    "go", "java", "kt", "kts", "c", "h", "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "zsh",
    "fish", "sql", "graphql", "proto", "plist", "pbxproj", "resolved", "workspace", "xcworkspacedata",
  ].includes(extension);
}

export function resourceTabTitle(tab: ResourceTab): string {
  switch (tab.type) {
    case "context": return "当前上下文";
    case "workspace": return "工作空间";
    case "text":
    case "markdown":
    case "image":
    case "pdf": return tab.resource.display_name;
    case "browser": return "浏览器";
    case "terminal": return "终端";
  }
}
