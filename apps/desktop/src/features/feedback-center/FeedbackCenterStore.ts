import { action, makeObservable, observable, runInAction } from "mobx";
import type { FeedbackResponse, FeedbackSupport, FeedbackTargetKind, SubmittedFeedback, SubmittedFeedbackPage, SubmittedFeedbackStatus, SubmitFeedbackRequest } from "@ez-assistant/protocol";
import { getDesktopPlatform } from "../../native-bridge/desktopLifecycle";
import type { RuntimeClient } from "../../runtime-client/RuntimeClient";
import { clientEnvironment, contentOf, emptyDraft, failures, type Draft } from "./model";

type Dependencies = { client: () => RuntimeClient | null; scope: () => string | null };
/** 当前连接内存草稿与视图。中心是反馈的唯一权威；取消等待不表示撤销写入。 */
export class FeedbackCenterStore {
  open = false;
  view: "list" | "form" | "detail" = "list";
  busy = false;
  message = "";
  support: FeedbackSupport | null = null;
  page: SubmittedFeedbackPage | null = null;
  detail: SubmittedFeedback | null = null;
  draft = emptyDraft();
  frozen: SubmitFeedbackRequest | null = null;
  unknown = false;
  checked = false;
  status: SubmittedFeedbackStatus | "" = "";
  kind: FeedbackTargetKind | "" = "";
  offset = 0;
  environment = clientEnvironment();
  plugins: { name: string; display_name: string; version: string | null }[] = [];
  #scope: string | null = null;
  #revision = 0;
  #disposed = false;
  #abort = new AbortController();

  constructor(private readonly dependencies: Dependencies) {
    makeObservable(this, { open: observable, view: observable, busy: observable, message: observable, support: observable,
      page: observable, detail: observable, draft: observable, frozen: observable, unknown: observable, checked: observable,
      environment: observable, status: observable, kind: observable, offset: observable, plugins: observable,
      setScope: action, show: action, close: action, edit: action, create: action, back: action, filter: action, dispose: action });
  }
  setScope(scope: string | null) {
    if (scope === this.#scope) return;
    this.#scope = scope; this.#revision++; this.#abort.abort(); this.#abort = new AbortController();
    this.open = false; this.busy = false; this.message = ""; this.support = null; this.page = null; this.detail = null;
    this.draft = emptyDraft(); this.frozen = null; this.unknown = false; this.checked = false; this.offset = 0; this.plugins = []; this.status = ""; this.kind = "";
  }
  show() { this.setScope(this.dependencies.scope()); this.open = true; this.view = "list"; void this.load(); }
  close() { if (this.busy) return; this.open = false; this.draft = emptyDraft(); this.frozen = null; this.unknown = false; this.message = ""; }
  edit(update: Partial<Draft>) { if (!this.busy && !this.unknown) { this.draft = { ...this.draft, ...update }; this.message = ""; } }
  create() { if (!this.busy) { this.draft = emptyDraft(); this.frozen = null; this.unknown = false; this.checked = false; this.message = ""; this.view = "form"; void this.loadPlugins(); } }
  back() { if (!this.busy) { this.view = "list"; this.draft = emptyDraft(); this.frozen = null; this.unknown = false; void this.load(); } }
  filter(status: SubmittedFeedbackStatus | "", kind: FeedbackTargetKind | "") { this.status = status; this.kind = kind; this.offset = 0; this.page = null; void this.load(); }
  get dirty() { return this.view === "form" && (this.unknown || JSON.stringify(this.draft) !== JSON.stringify(emptyDraft())); }
  get content_bytes() { const content = contentOf(this.draft); return content ? new TextEncoder().encode(JSON.stringify(content)).length : 0; }

  async #run(work: (client: RuntimeClient, valid: () => boolean, signal: AbortSignal) => Promise<void>, writing = false) {
    if (this.busy || this.#disposed) return;
    const client = this.dependencies.client(); const scope = this.dependencies.scope();
    if (!client || scope !== this.#scope) { this.setScope(scope); return; }
    const revision = ++this.#revision;
    const valid = () => !this.#disposed && revision === this.#revision && scope === this.dependencies.scope() && client === this.dependencies.client();
    runInAction(() => { this.busy = true; this.message = ""; });
    const timeout = new AbortController();
    const timer = globalThis.setTimeout(() => timeout.abort(), 20_000);
    try { await work(client, valid, AbortSignal.any([this.#abort.signal, timeout.signal])); }
    catch { if (valid()) runInAction(() => { this.message = writing ? failures.submission_unknown : failures.unavailable; if (writing) { this.unknown = true; this.checked = false; } }); }
    finally { globalThis.clearTimeout(timer); if (!this.#disposed && revision === this.#revision && scope === this.dependencies.scope()) runInAction(() => {
      this.busy = false;
      if (writing && client !== this.dependencies.client()) { this.unknown = true; this.checked = false; this.message = failures.submission_unknown; }
    }); }
  }
  #accept<T>(response: FeedbackResponse<T>, apply: (value: T) => void) {
    runInAction(() => { if (response.status === "success") apply(response.data); else this.message = failures[response.data]; });
  }
  load(offset = this.offset) { return this.#run(async (client, valid, signal) => {
    const platform = await getDesktopPlatform().catch(() => "unsupported" as const);
    if (!valid()) return;
    runInAction(() => { this.environment = { ...clientEnvironment(), client_os: platform === "unsupported" ? null : platform }; });
    const supported = await client.command({ type: "get_feedback_support", payload: {} }, { signal });
    if (!valid()) return;
    this.#accept(supported.payload, (support) => { this.support = support; if (!support.available) this.message = failures.unsupported; });
    if (supported.payload.status !== "success" || !supported.payload.data.available) return;
    const response = await client.command({ type: "list_submitted_feedback", payload: { offset, status: this.status || undefined, target_kind: this.kind || undefined } }, { signal });
    if (valid()) this.#accept(response.payload, (page) => { this.page = page; this.offset = offset; });
  }); }
  read(id: string) { return this.#run(async (client, valid, signal) => {
    const response = await client.command({ type: "get_submitted_feedback", payload: { id } }, { signal });
    if (valid()) this.#accept(response.payload, (detail) => { this.detail = detail; this.view = "detail"; });
  }); }
  submit() {
    if (this.busy) return Promise.resolve();
    const content = contentOf(this.draft);
    if (!content) { runInAction(() => { this.message = "请选择反馈目标，并补全具体内容的位置。"; }); return Promise.resolve(); }
    if (this.content_bytes > 12 * 1024) { runInAction(() => { this.message = failures.too_large; }); return Promise.resolve(); }
    if (this.unknown && !this.checked) return Promise.resolve();
    if (!this.frozen || JSON.stringify(content) !== JSON.stringify(this.frozen.content)) {
      runInAction(() => { this.frozen = { submission_id: crypto.randomUUID(), content, client: this.environment }; });
    }
    const request = this.frozen!;
    return this.#run(async (client, valid, signal) => {
      const response = await client.command({ type: "submit_feedback", payload: request }, { signal });
      if (!valid()) return;
      this.#accept(response.payload, (detail) => { this.detail = detail; this.view = "detail"; this.draft = emptyDraft(); this.frozen = null; this.unknown = false; this.message = "反馈已提交。"; });
      if (response.payload.status === "failure") runInAction(() => { this.unknown = response.payload.data === "submission_unknown" || response.payload.data === "conflict"; this.checked = false; });
    }, true);
  }
  checkSubmission() { return this.#run(async (client, valid, signal) => {
    if (!this.frozen) return;
    const response = await client.command({ type: "list_submitted_feedback", payload: { offset: 0, submission_id: this.frozen.submission_id } }, { signal });
    if (!valid()) return;
    if (response.payload.status === "failure") { this.#accept(response.payload, () => {}); return; }
    const item = response.payload.data.items[0];
    if (!item) { runInAction(() => { this.checked = true; this.message = "暂未查到，可重试本次提交。原请求仍可能已保存。"; }); return; }
    const detail = await client.command({ type: "get_submitted_feedback", payload: { id: item.id } }, { signal });
    if (valid()) this.#accept(detail.payload, (value) => { this.detail = value; this.view = "detail"; this.unknown = false; this.frozen = null; this.draft = emptyDraft(); this.message = "已找到本次提交。"; });
  }); }
  async loadPlugins() {
    const client = this.dependencies.client(); const scope = this.#scope;
    if (!client) return;
    const results = await Promise.allSettled([client.command({ type: "list_plugins", payload: {} }), client.command({ type: "list_plugin_market", payload: {} })]);
    if (this.#disposed || scope !== this.dependencies.scope() || client !== this.dependencies.client()) return;
    const entries = new Map<string, { name: string; display_name: string; version: string | null }>();
    if (results[1].status === "fulfilled") for (const plugin of results[1].value.payload.plugins) entries.set(plugin.name, { ...plugin, version: null });
    if (results[0].status === "fulfilled") for (const plugin of results[0].value.payload.plugins) entries.set(plugin.name, { name: plugin.name, display_name: plugin.display_name, version: plugin.version });
    runInAction(() => { this.plugins = [...entries.values()]; });
  }
  dispose() { this.#disposed = true; this.setScope(null); this.#abort.abort(); }
}
