import type {
  ConversationItem, ConversationNavigationItem, ConversationNavigationPage, ConversationOwner,
  MessageId, RuntimeEvent,
} from "@ez-assistant/protocol";
import { makeObservable, observable, observableRef, reaction, runInAction } from "mobx";
import { RuntimeClientError, type RuntimeClient } from "../runtime-client/RuntimeClient";
import type { RuntimeLifecycleCoordinator, RuntimeReadQuery } from "./RuntimeLifecycleCoordinator";

const PAGE_SIZE = 128;
const MAX_PAGES = 4;
const MAX_PREVIEWS = 256;
const PREVIEW_LENGTH = 160;

type Preview = Readonly<{ text: string; truncated: boolean }>;
export type PreviewSlot = Readonly<
  { status: "loading" }
  | { status: "ready"; value: Preview }
  | { status: "error"; message: string }
>;
type NavigationPreview = Readonly<{
  user_message_id: MessageId;
  user: PreviewSlot;
  assistant: PreviewSlot | null;
}>;
type NavigationPage = Readonly<{ cursor: string | null; value: ConversationNavigationPage; stale: boolean }>;
type Scope = Readonly<{
  owner: ConversationOwner;
  generation: number;
  client: RuntimeClient;
  runtime: Pick<RuntimeLifecycleCoordinator, "readConversation">;
}>;
export type NavigationDirection = "latest" | "older" | "newer";
type PageIntent = NavigationDirection | NavigationPage;
type PageRead = { controller: AbortController; intent: PageIntent; promise: Promise<boolean> };

export function sameConversationOwner(a: ConversationOwner, b: ConversationOwner): boolean {
  return a.session_id === b.session_id && (a.type === "main_session"
    ? b.type === "main_session" : b.type === "child_task" && a.child_task_id === b.child_task_id);
}

function sameScope(a: Scope | null, b: Scope | null): boolean {
  return a === b || Boolean(a && b && a.client === b.client && a.runtime === b.runtime
    && a.generation === b.generation && sameConversationOwner(a.owner, b.owner));
}

/** 仅保存活动 owner 的可恢复页面和短文本，读取与 UI 布局相互独立。 */
export class ConversationNavigationStore {
  pages: readonly NavigationPage[] = [];
  loading_page = false;
  page_error: string | null = null;
  scope_revision = 0;
  page_direction: NavigationDirection | null = null;
  preview: NavigationPreview | null = null;
  #scope: Scope | null = null;
  #enabled = false;
  #visible: ReadonlySet<string | null> = new Set();
  #visible_anchor: MessageId | null = null;
  #follow_latest = false;
  #failed_intent: PageIntent | null = null;
  #refresh_scheduled = false;
  #page_read: PageRead | null = null;
  #preview_read: AbortController | null = null;
  #preview_id: MessageId | null = null;
  #cache = new Map<MessageId, Preview>();
  #stop: () => void;

  constructor(
    private readonly getScope: () => Scope | null,
    private readonly getHistory: () => readonly ConversationItem[],
  ) {
    makeObservable(this, {
      pages: observableRef, loading_page: observable, page_error: observable, preview: observableRef,
      scope_revision: observable, page_direction: observable,
    });
    this.#stop = reaction(getScope, scope => {
      this.suspend();
      this.#scope = scope;
      this.#failed_intent = null;
      runInAction(() => { this.pages = []; this.page_error = null; this.page_direction = null; this.scope_revision++; });
      this.#cache.clear();
    }, { fireImmediately: true, equals: sameScope });
  }

  /** 可见时由视图显式启动读取，隐藏时保留页面和可恢复游标。 */
  activate(): void {
    this.#enabled = true;
    this.#scheduleRefresh();
  }

  suspend(): void {
    this.#enabled = false;
    this.#visible = new Set();
    this.#visible_anchor = null;
    this.#follow_latest = false;
    this.#cancelPage();
    this.closePreview();
  }

  dispose(): void {
    this.#stop();
    this.suspend();
    this.#cache.clear();
  }

  setVisiblePages(cursors: readonly (string | null)[], follow_latest = false, anchor: MessageId | null = null): void {
    this.#visible = new Set(cursors);
    this.#follow_latest = follow_latest;
    this.#visible_anchor = anchor;
    this.#scheduleRefresh();
  }

  loadPage(direction: NavigationDirection = "latest"): Promise<boolean> {
    if (this.#page_read?.intent === direction) return this.#page_read.promise;
    return this.#startPage(direction);
  }

  retryPage(): Promise<boolean> {
    return this.#failed_intent ? this.#startPage(this.#failed_intent) : Promise.resolve(false);
  }

  async confirmUser(id: MessageId): Promise<boolean> {
    const scope = this.#scope;
    if (!scope || !this.#current(scope)) return false;
    const existing = this.#pageWithUser(id);
    if (existing && !existing.stale) return true;
    await this.#page_read?.promise;
    if (!this.#current(scope)) return false;
    const page = this.#pageWithUser(id);
    if (!page?.stale) return Boolean(page);
    if (!await this.#startPage(page)) throw new Error(this.page_error ?? "缩略历史正在更新，请重试。");
    return this.#pageWithUser(id)?.stale === false;
  }

  /** 可靠变化使本 owner 的读取投影过期。流式增量不单独触发预览。 */
  observe(event: RuntimeEvent, client: RuntimeClient | null): void {
    const scope = this.#scope;
    if (!scope || scope.client !== client || !affectsNavigation(event, scope.owner)) return;
    this.#cache.clear();
    this.#cancelPage();
    this.#preview_read?.abort();
    runInAction(() => {
      const stale = this.pages.map(page => ({ ...page, stale: true }));
      const failed_index = typeof this.#failed_intent === "object" && this.#failed_intent
        ? this.pages.indexOf(this.#failed_intent) : -1;
      if (failed_index >= 0) this.#failed_intent = stale[failed_index];
      this.pages = stale;
      this.preview = null;
    });
    this.#scheduleRefresh();
    const id = this.#preview_id;
    if (id) queueMicrotask(() => {
      if (this.#enabled && this.#preview_id === id && !this.preview) void this.openPreview(id);
    });
  }

  closePreview(): void {
    this.#preview_read?.abort();
    this.#preview_read = null;
    this.#preview_id = null;
    runInAction(() => { this.preview = null; });
  }

  cachedPreview(id: MessageId): PreviewSlot | undefined {
    const value = this.#cache.get(id);
    return value ? { status: "ready", value } : undefined;
  }

  async openPreview(user_message_id: MessageId, retry_side?: "user" | "assistant"): Promise<void> {
    const previous = this.preview?.user_message_id === user_message_id ? this.preview : null;
    this.closePreview();
    const scope = this.#scope;
    if (!scope || !this.#current(scope)) return;
    this.#preview_id = user_message_id;
    const controller = new AbortController();
    this.#preview_read = controller;
    const current = () => this.#preview_read === controller && !controller.signal.aborted && this.#current(scope);
    runInAction(() => {
      this.preview = previous && retry_side ? { ...previous, [retry_side]: { status: "loading" } }
        : { user_message_id, user: { status: "loading" }, assistant: null };
    });
    try {
      await scope.runtime.readConversation(async (query, signal) => {
        await this.#page_read?.promise;
        signal.throwIfAborted();
        let page = this.#pageWithUser(user_message_id);
        if (page?.stale) {
          if (!await this.#startPage(page, query)) {
            throw new RuntimeClientError("conversation_unavailable", this.page_error ?? "缩略历史正在更新，请重试。");
          }
          page = this.#pageWithUser(user_message_id);
        }
        const item = page?.value.items.find(item => item.user_message_id === user_message_id);
        if (!item || page?.stale) throw new RuntimeClientError("message_not_found", "该消息的预览已不可用。");
        if (!current()) return;
        const assistant_id = item.assistant_message_id;
        if (!previous || !retry_side) runInAction(() => {
          this.preview = { user_message_id, user: { status: "loading" }, assistant: assistant_id ? { status: "loading" } : null };
        });
        await Promise.all([
          ...(!previous || !retry_side || retry_side === "user" || previous.user.status === "loading"
            ? [this.#previewSlot(query, scope, item, "user", current)] : []),
          ...(assistant_id && (!previous || !retry_side || retry_side === "assistant" || previous.assistant?.status === "loading")
            ? [this.#previewSlot(query, scope, item, "assistant", current)] : []),
        ]);
      }, controller.signal);
    } catch (error) {
      if (current()) runInAction(() => {
        this.preview = { user_message_id, user: { status: "error", message: failure(error) }, assistant: null };
      });
    } finally {
      if (this.#preview_read === controller) this.#preview_read = null;
    }
  }

  async #previewSlot(
    query: RuntimeReadQuery, scope: Scope, item: ConversationNavigationItem,
    side: "user" | "assistant", current: () => boolean,
  ): Promise<void> {
    const message_id = side === "user" ? item.user_message_id : item.assistant_message_id;
    if (!message_id) return;
    let slot: PreviewSlot;
    try {
      let value = this.#cache.get(message_id);
      if (!value) {
        const local = this.getHistory().find(message => message.message_id === message_id && message.type === side);
        value = local && localPreview(local);
      }
      if (!value) {
        const result = await query({ type: "get_conversation_message_preview", payload: {
          owner: scope.owner, generation: scope.generation, message_id,
        } });
        const received = result.payload.snapshot.value;
        if (!sameConversationOwner(received.owner, scope.owner) || received.generation !== scope.generation
          || received.message_id !== message_id) {
          throw new RuntimeClientError("snapshot_stale", "消息预览已更新，请重试。");
        }
        value = { text: received.text, truncated: received.truncated };
      }
      if (!current()) return;
      this.#cache.delete(message_id);
      this.#cache.set(message_id, value);
      if (this.#cache.size > MAX_PREVIEWS) {
        const oldest = this.#cache.keys().next().value;
        if (oldest !== undefined) this.#cache.delete(oldest);
      }
      slot = { status: "ready", value };
    } catch (error) {
      if (!current()) return;
      slot = { status: "error", message: failure(error) };
    }
    runInAction(() => {
      if (this.preview && current()) this.preview = { ...this.preview, [side]: slot };
    });
  }

  #pageWithUser(id: MessageId): NavigationPage | undefined {
    return this.pages.find(page => page.value.items.some(item => item.user_message_id === id));
  }

  #current(scope: Scope): boolean {
    return this.#enabled && this.#scope === scope && sameScope(scope, this.getScope());
  }

  #cancelPage(): void {
    this.#page_read?.controller.abort();
    this.#page_read = null;
    runInAction(() => { this.loading_page = false; });
  }

  #startPage(intent: PageIntent, existing_query?: RuntimeReadQuery): Promise<boolean> {
    const scope = this.#scope;
    if (!scope || !this.#current(scope) || this.#page_read) return Promise.resolve(false);
    const pages = this.pages;
    const index = typeof intent === "string" ? -1 : pages.indexOf(intent);
    if (typeof intent !== "string" && index < 0) return Promise.resolve(false);
    // 已上翻时用相邻页的向后游标重验原窗口，避免最新页随新增输入移走。
    const cursor = typeof intent !== "string" ? intent.cursor ?? pages[index - 1]?.value.next_cursor ?? null
      : intent === "older" ? pages[0]?.value.previous_cursor
      : intent === "newer" ? pages.at(-1)?.value.next_cursor : null;
    if ((intent === "older" || intent === "newer") && !cursor) return Promise.resolve(false);
    const controller = new AbortController();
    const read: PageRead = { controller, intent, promise: Promise.resolve(false) };
    this.#page_read = read;
    const current = () => this.#page_read === read && !controller.signal.aborted && this.#current(scope);
    this.#failed_intent = null;
    runInAction(() => {
      this.loading_page = true;
      this.page_error = null;
      this.page_direction = typeof intent === "string" ? intent : index === 0 && pages.length > 1 ? "older" : "newer";
    });
    const readPage = async (query: RuntimeReadQuery, cursor: string | null): Promise<NavigationPage> => {
      const result = await query({ type: "list_conversation_navigation", payload: {
        owner: scope.owner, cursor, limit: PAGE_SIZE,
      } });
      const value = result.payload.snapshot.value;
      if (!sameConversationOwner(value.owner, scope.owner) || value.generation !== scope.generation) {
        throw new RuntimeClientError("snapshot_stale", "缩略历史已更新，请重试。");
      }
      if (value.items.length > PAGE_SIZE || (cursor && (!value.items.length
        || (intent === "older" && value.previous_cursor === cursor)
        || (intent === "newer" && value.next_cursor === cursor)))) {
        throw new RuntimeClientError("protocol_mismatch", "缩略历史未能继续翻页。");
      }
      return { cursor, value, stale: false };
    };
    const queryPage = async (query: RuntimeReadQuery) => {
      let page = await readPage(query, cursor ?? null);
      if (!current()) return false;
      let preceding: NavigationPage | null = null;
      const anchor = this.#visible_anchor;
      if (typeof intent !== "string" && cursor === null && !this.#follow_latest
        && anchor && intent.value.items.some(item => item.user_message_id === anchor)
        && !page.value.items.some(item => item.user_message_id === anchor) && page.value.previous_cursor) {
        // 末页滑过阅读锚点时先找回旧标记，再原子替换窗口。
        const prior = await readPage(query, page.value.previous_cursor);
        if (!current()) return false;
        if (prior.value.items.some(item => item.user_message_id === anchor)) {
          preceding = prior;
        } else if (intent.value.previous_cursor) {
          const boundary = await readPage(query, intent.value.previous_cursor);
          if (!current()) return false;
          if (boundary.value.next_cursor) {
            const anchored = await readPage(query, boundary.value.next_cursor);
            if (anchored.value.items.some(item => item.user_message_id === anchor)) {
              page = anchored;
              preceding = boundary;
            }
          }
        }
      }
      if (!current()) return false;
      runInAction(() => {
        if (intent === "older") this.pages = [page, ...pages].slice(0, MAX_PAGES);
        else if (intent === "newer") this.pages = [...pages, page].slice(-MAX_PAGES);
        else if (intent === "latest") this.pages = [page];
        else if (preceding) this.pages = [preceding, page];
        else this.pages = pages.map(candidate => candidate === intent ? page : candidate);
      });
      return true;
    };
    read.promise = (async () => {
      let success = false;
      try {
        success = existing_query ? await queryPage(existing_query) : await scope.runtime.readConversation(queryPage, controller.signal);
        return success;
      } catch (error) {
        if (current()) runInAction(() => { this.#failed_intent = intent; this.page_error = failure(error); });
        return false;
      } finally {
        if (this.#page_read === read) {
          this.#page_read = null;
          runInAction(() => { this.loading_page = false; });
          if (success) this.#scheduleRefresh();
        }
      }
    })();
    return read.promise;
  }

  #scheduleRefresh(): void {
    if (!this.#enabled || this.#refresh_scheduled || this.#page_read || this.page_error) return;
    this.#refresh_scheduled = true;
    queueMicrotask(() => {
      this.#refresh_scheduled = false;
      if (!this.#enabled || this.#page_read || this.page_error) return;
      const page = this.pages.find(page => page.stale && (this.#visible.has(page.cursor) || !page.value.items.length));
      if (page) void this.#startPage(page);
      else if (this.#follow_latest && this.pages.at(-1)?.value.next_cursor) void this.#startPage("newer");
    });
  }
}

function localPreview(message: ConversationItem): Preview | undefined {
  const parts = message.type === "user" ? [message.text] : message.type === "assistant"
    ? message.segments.flatMap(segment => segment.type === "text" && segment.text.trim() ? [segment.text] : [])
    : null;
  if (!parts) return undefined;
  let text = "";
  let remaining = PREVIEW_LENGTH;
  for (const [index, part] of parts.entries()) {
    for (const chunk of [index === 0 ? "" : "\n\n", part]) {
      for (const ch of chunk) {
        if (remaining === 0) return { text, truncated: true };
        text += ch;
        remaining--;
      }
    }
  }
  return { text, truncated: false };
}

function affectsNavigation(event: RuntimeEvent, owner: ConversationOwner): boolean {
  switch (event.type) {
    case "conversation_committed":
    case "step_committed":
      return sameConversationOwner(event.owner, owner);
    case "child_task_event":
      return owner.type === "child_task" && owner.session_id === event.session_id
        && owner.child_task_id === event.child_task_id && ["started", "step_started", "finished"].includes(event.event.type);
    case "run_accepted":
    case "run_started":
    case "run_cancelling":
    case "run_finished":
    case "step_started":
      return owner.type === "main_session" && owner.session_id === event.session_id;
    default:
      return false;
  }
}

function failure(error: unknown): string {
  return error instanceof Error ? error.message : "缩略消息读取失败，请重试。";
}
