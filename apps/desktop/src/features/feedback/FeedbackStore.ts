import { action, makeObservable, observable, runInAction } from 'mobx';
import type { FeedbackAnswer, FeedbackRequestSnapshot, RuntimeEventEnvelope } from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../runtime-client/RuntimeClient';

type Draft = Readonly<{ answers: FeedbackAnswer[]; active: number }>;
type Dependencies = Readonly<{ client: () => RuntimeClient | null; scope: () => string | null }>;

/** 权威问题来自快照/事件；未提交答案只保存在当前用户与 Runtime 的内存草稿中。 */
export class FeedbackStore {
  readonly requests = observable.map<string, FeedbackRequestSnapshot>(undefined, { deep: false });
  readonly drafts = observable.map<string, Draft>(undefined, { deep: false });
  readonly pending = observable.map<string, 'answer' | 'cancel'>();
  readonly errors = observable.map<string, string>();
  focus_id: string | null = null;
  focus_revision = 0;
  #scope: string | null = null;
  #disposed = false;
  #loads = new Map<string, Promise<void>>();
  #revisions = new Map<string, number>();

  constructor(private readonly dependencies: Dependencies) {
    makeObservable(this, {
      focus_id: observable, focus_revision: observable, setScope: action, ingest: action,
      setDraft: action, open: action, dispose: action,
    });
  }
  setScope(scope: string | null): void {
    if (scope === this.#scope) return;
    this.#scope = scope;
    this.requests.clear(); this.drafts.clear(); this.pending.clear(); this.errors.clear();
    this.#loads.clear(); this.#revisions.clear(); this.focus_id = null;
  }
  forSession(session_id: string): FeedbackRequestSnapshot[] {
    return [...this.requests.values()].filter((request) => request.session_id === session_id);
  }
  waiting(session_id: string): FeedbackRequestSnapshot | undefined {
    return this.forSession(session_id).find((request) => request.status === 'pending' || this.pending.has(request.request_id));
  }
  draft(request: FeedbackRequestSnapshot): Draft {
    return this.drafts.get(request.request_id) ?? {
      answers: request.questions.map(() => ({ selected_options: [], text: '' })), active: 0,
    };
  }
  setDraft(request: FeedbackRequestSnapshot, update: Partial<Draft>): void {
    this.drafts.set(request.request_id, { ...this.draft(request), ...update });
  }
  open(request: FeedbackRequestSnapshot): void {
    this.focus_id = request.request_id;
    this.focus_revision += 1;
  }
  ingest(envelope: RuntimeEventEnvelope): void {
    const event = envelope.event;
    if (event.type === 'session_deleted') {
      this.#revisions.set(event.session_id, (this.#revisions.get(event.session_id) ?? 0) + 1);
      for (const request of this.forSession(event.session_id)) {
        this.requests.delete(request.request_id); this.drafts.delete(request.request_id); this.errors.delete(request.request_id);
      }
      return;
    }
    if (event.type !== 'feedback_published' && event.type !== 'feedback_changed') return;
    const request = event.request;
    this.#revisions.set(request.session_id, (this.#revisions.get(request.session_id) ?? 0) + 1);
    this.requests.set(request.request_id, request);
    // 已接受答案或取消后才清理；一次发送失败不会触碰草稿。
    if (request.status !== 'pending' && !this.pending.has(request.request_id)) this.drafts.delete(request.request_id);
  }
  refresh(session_id: string): Promise<void> {
    const previous = this.#loads.get(session_id);
    if (previous) return previous;
    const client = this.dependencies.client();
    const scope = this.dependencies.scope();
    if (!client || !scope || this.#disposed) return Promise.resolve();
    const revision = this.#revisions.get(session_id) ?? 0;
    const promise = client.command({ type: 'list_feedback_requests', payload: { session_id } }).then((result) => {
      if (this.#disposed || client !== this.dependencies.client() || scope !== this.dependencies.scope()
        || revision !== (this.#revisions.get(session_id) ?? 0)) return;
      runInAction(() => {
        for (const request of this.forSession(session_id)) this.requests.delete(request.request_id);
        for (const request of result.payload) {
          this.requests.set(request.request_id, request);
          if (request.status !== 'pending' && !this.pending.has(request.request_id)) this.drafts.delete(request.request_id);
        }
        for (const id of this.drafts.keys()) if (!this.requests.has(id)) this.drafts.delete(id);
        this.errors.delete(session_id);
      });
    }).catch((error: unknown) => {
      if (!this.#disposed && scope === this.dependencies.scope()) runInAction(() => this.errors.set(session_id, message(error)));
    }).finally(() => {
      if (this.#loads.get(session_id) === promise) this.#loads.delete(session_id);
    });
    this.#loads.set(session_id, promise);
    return promise;
  }
  async answer(request: FeedbackRequestSnapshot): Promise<boolean> {
    return this.#mutate(request, false);
  }
  async cancel(request: FeedbackRequestSnapshot): Promise<boolean> {
    return this.#mutate(request, true);
  }
  async #mutate(request: FeedbackRequestSnapshot, cancel: boolean): Promise<boolean> {
    const client = this.dependencies.client();
    const scope = this.dependencies.scope();
    if (!client || !scope || this.pending.has(request.request_id) || this.#disposed) return false;
    const answers = this.draft(request).answers;
    runInAction(() => { this.pending.set(request.request_id, cancel ? 'cancel' : 'answer'); this.errors.delete(request.request_id); });
    try {
      const result = cancel
        ? await client.command({ type: 'cancel_feedback', payload: { session_id: request.session_id, request_id: request.request_id } })
        : await client.command({ type: 'answer_feedback', payload: { session_id: request.session_id, request_id: request.request_id, answers } });
      if (this.#disposed || client !== this.dependencies.client() || scope !== this.dependencies.scope()) return false;
      runInAction(() => {
        this.#revisions.set(request.session_id, (this.#revisions.get(request.session_id) ?? 0) + 1);
        this.requests.set(request.request_id, result.payload);
        this.drafts.delete(request.request_id);
      });
      return true;
    } catch (error) {
      if (!this.#disposed && scope === this.dependencies.scope()) runInAction(() => this.errors.set(request.request_id, message(error)));
      return false;
    } finally {
      if (!this.#disposed && scope === this.dependencies.scope()) runInAction(() => this.pending.delete(request.request_id));
    }
  }
  dispose(): void {
    this.#disposed = true; this.requests.clear(); this.drafts.clear(); this.pending.clear(); this.errors.clear(); this.#loads.clear();
  }
}
function message(error: unknown): string { return error instanceof Error ? error.message : '提交失败，请重试。'; }
export function answerComplete(answer: FeedbackAnswer): boolean { return answer.selected_options.length > 0 || answer.text.trim().length > 0; }
