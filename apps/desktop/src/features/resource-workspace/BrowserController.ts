import { action, makeObservable, observable, runInAction } from "mobx";
import {
  actOnResourceBrowser, closeResourceBrowser, createResourceBrowser,
  navigateResourceBrowser, resourceBrowserUrl,
  type BrowserAction, type BrowserEvent,
} from "../../native-bridge/resourceBrowser";

/** Desktop 浏览器的 UI 投影；关闭标签也会等待并释放尚在创建中的原生句柄。 */
export class BrowserController {
  native_id: string | null = null;
  url = "";
  title = "新标签页";
  loading = false;
  load_delayed = false;
  error: string | null = null;
  notice: string | null = null;
  notice_url: string | null = null;
  #disposed = false;
  #page: object | null = {};
  // 页面实例会跨导航保留；单独取消旧 URL 查询，null 表示新地址尚未获得原生页面事件。
  #url_sync: object | null = {};
  #pending: Promise<void> = Promise.resolve();
  #load_timer: ReturnType<typeof setTimeout> | undefined;
  readonly #on_popup: (url: string) => void;
  readonly #on_close_error: (message: string) => void;

  constructor(on_popup: (url: string) => void, on_close_error: (message: string) => void) {
    this.#on_popup = on_popup;
    this.#on_close_error = on_close_error;
    makeObservable(this, {
      native_id: observable, url: observable, title: observable, loading: observable, load_delayed: observable,
      error: observable, notice: observable, notice_url: observable, navigate: action, perform: action,
      reportError: action, reportNotice: action, dismissNotice: action, suspend: action, resume: action,
    });
  }

  navigate(value: string): boolean {
    if (this.#disposed) return false;
    let url: string;
    try { url = browserAddress(value); } catch (failure) { this.reportNotice(failure); return false; }
    this.#url_sync = null;
    this.url = url;
    this.error = null;
    this.notice = null;
    this.notice_url = null;
    const page = this.#page;
    if (!page) return true;
    this.#queue(async () => {
      runInAction(() => this.#startLoading());
      if (this.native_id) await navigateResourceBrowser(this.native_id, url);
      else {
        const id = await createResourceBrowser(url, (event) => { if (this.#page === page) this.#receive(event); });
        // 即使标签已关闭也接住句柄，由 dispose 排在创建之后的关闭操作负责释放。
        runInAction(() => { this.native_id = id; });
      }
    });
    return true;
  }

  perform(action: BrowserAction): void {
    if (!this.native_id || this.#disposed || !this.#page) return;
    if (action !== "focus") this.#url_sync = {};
    this.error = null;
    this.#queue(async () => {
      if (!this.native_id) return;
      if (action === "reload") runInAction(() => this.#startLoading());
      await actOnResourceBrowser(this.native_id, action);
      if (action === "stop") runInAction(() => this.#stopLoading());
    });
  }

  async refreshUrl(): Promise<void> {
    const id = this.native_id;
    const page = this.#page;
    if (!id || this.#disposed || !page || !this.#url_sync) return;
    const sync = this.#url_sync = {};
    const is_current = () => !this.#disposed && this.#page === page
      && this.native_id === id && this.#url_sync === sync;
    try {
      const url = await resourceBrowserUrl(id);
      if (is_current()) runInAction(() => { this.url = url; });
    } catch (failure) { if (is_current()) this.reportError(failure); }
  }

  reportError(failure: unknown): void {
    this.#stopLoading();
    this.error = failure instanceof Error ? failure.message : String(failure);
  }

  /** 地址输入、系统打开等操作失败只提示，不改变当前网页或它的加载状态。 */
  reportNotice(failure: unknown): void {
    if (this.#disposed) return;
    this.notice = failure instanceof Error ? failure.message : String(failure);
    this.notice_url = null;
  }

  dismissNotice(): void { this.notice = null; this.notice_url = null; this.load_delayed = false; }

  /** 只释放页面实例，保留最后 URL/标题；创建和关闭串行，迟到页面事件不能覆盖重建实例。 */
  suspend(): void {
    if (!this.#page) return;
    this.#page = null;
    this.#url_sync = null;
    this.#stopLoading();
    this.#pending = this.#pending.then(async () => {
      const id = this.native_id;
      if (id) await closeResourceBrowser(id);
      runInAction(() => { this.native_id = null; });
    }).catch((failure: unknown) => this.#on_close_error(`关闭网页失败：${String(failure)}`));
  }

  resume(): void {
    if (this.#disposed || this.#page) return;
    this.#page = {};
    if (this.url) this.navigate(this.url);
  }

  dispose(): void {
    if (this.#disposed) return;
    this.#disposed = true;
    this.suspend();
  }

  #queue(operation: () => Promise<void>): void {
    const page = this.#page;
    this.#pending = this.#pending.then(async () => {
      if (!this.#disposed && page && this.#page === page) await operation();
    }).catch((failure: unknown) => { if (!this.#disposed && this.#page === page) this.reportError(failure); });
  }

  #receive(event: BrowserEvent): void {
    if (this.#disposed) return;
    runInAction(() => {
      switch (event.type) {
        case "load_started": this.#url_sync = {}; this.url = event.url; this.error = null; this.#startLoading(); break;
        case "loaded": this.#url_sync = {}; this.url = event.url; this.error = null; this.#stopLoading(); break;
        case "title": this.title = event.title || "浏览器"; break;
        case "popup": this.#on_popup(event.url); break;
        case "notice": this.notice = event.message; this.notice_url = event.url; this.#stopLoading(); break;
      }
    });
  }

  #startLoading(): void {
    this.loading = true;
    this.load_delayed = false;
    clearTimeout(this.#load_timer);
    // 未完成可能只是子资源缓慢；超时仅结束进度动画并提示，不能当作错误隐藏已呈现的网页。
    this.#load_timer = setTimeout(() => runInAction(() => {
      this.#stopLoading();
      this.load_delayed = true;
    }), 30_000);
  }

  #stopLoading(): void {
    this.loading = false;
    this.load_delayed = false;
    clearTimeout(this.#load_timer);
  }
}

export function browserAddress(value: string): string {
  const input = value.trim();
  if (!input) throw new Error("请输入网页地址。");
  const explicit_scheme = /^[a-z][a-z\d+.-]*:/i.test(input) && !/^[\w.-]+:\d+(?:\/|$)/.test(input);
  const candidate = explicit_scheme ? input : `${/^(localhost|127\.0\.0\.1|\[::1\])(?::|\/|$)/.test(input) ? "http" : "https"}://${input}`;
  let url: URL;
  try { url = new URL(candidate); } catch { throw new Error("请输入有效的网页地址。"); }
  if (!["http:", "https:"].includes(url.protocol) || !url.hostname || url.username || url.password) {
    throw new Error("只支持不含用户名和密码的 HTTP 或 HTTPS 地址。");
  }
  return url.toString();
}
