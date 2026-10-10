import { action, computed, makeObservable, observable, observableRef, runInAction } from 'mobx';
import type { ApprovalSnapshot, FeedbackRequestSnapshot, RuntimeEventEnvelope } from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../runtime-client/RuntimeClient';
import { notificationFromEvent, notificationKey, type NotificationTarget } from './types';
import { notificationText } from './content';

type PendingItem = Readonly<{ target: NotificationTarget; label: string }>;
type Dependencies = Readonly<{
  client: () => Pick<RuntimeClient, 'command'> | null;
  selectedSession: () => string | null;
  title: (session_id: string) => string | undefined;
  feedback: {
    requests: ReadonlyMap<string, FeedbackRequestSnapshot>;
    errors: ReadonlyMap<string, string>;
    refresh: (session_id: string) => Promise<void>;
  };
}>;

/** 仅保存既有查询/事件的待办投影与短提示；不持久化，不执行审批或回答。 */
export class PendingNotificationStore {
  readonly approvals = observable.map<string, ApprovalSnapshot>(undefined, { deep: false });
  readonly sessions = observable.map<string, string>();
  scope: string | null = null;
  ready = false;
  syncing = false;
  error = false;
  hint: PendingItem | null = null;
  #generation = 0;
  #catalog_revision = 0;
  #revisions = new Map<string, number>();
  #timer: ReturnType<typeof setTimeout> | null = null;
  #load: Promise<void> | null = null;
  #disposed = false;

  constructor(private readonly dependencies: Dependencies) {
    makeObservable(this, {
      scope: observable, ready: observable, syncing: observable, error: observable,
      hint: observableRef, items: computed, groups: computed,
      setConnection: action, ingest: action, dismissHint: action, dispose: action,
    });
  }

  get items(): PendingItem[] {
    if (!this.scope) return [];
    const scope = this.scope;
    const approvals = [...this.approvals.values()].map((approval) => ({
      target: notificationFromEvent(scope, { sequence: 0, emitted_at_ms: 0,
        event: { type: 'approval_requested', approval } })!.target,
      label: `需要审批：${approval.subject.type === 'mcp' ? approval.subject.identity.tool_name : approval.subject.tool_name}`,
    }));
    const feedback: PendingItem[] = [...this.dependencies.feedback.requests.values()]
      .filter((request) => request.status === 'pending' && this.sessions.has(request.session_id))
      .map((request) => ({
        target: { kind: 'feedback', scope, owner: { type: 'main_session', session_id: request.session_id },
          run_id: request.run_id, request_id: request.request_id },
        label: `等待回答：${notificationText(request.questions[0]?.question ?? '请查看问题', 100)}`,
      }));
    return [...approvals, ...feedback];
  }

  get groups(): ReadonlyArray<{ session_id: string; title: string; items: PendingItem[] }> {
    const groups = new Map<string, { session_id: string; title: string; items: PendingItem[] }>();
    for (const item of this.items) {
      const session_id = item.target.owner.session_id;
      let group = groups.get(session_id);
      if (!group) {
        group = { session_id, title: this.title(session_id), items: [] };
        groups.set(session_id, group);
      }
      group.items.push(item);
    }
    return [...groups.values()];
  }

  title(session_id: string): string {
    return this.dependencies.title(session_id) ?? this.sessions.get(session_id) ?? '其他会话';
  }

  setConnection(scope: string | null, ready: boolean): void {
    if (this.#disposed || (scope === this.scope && ready === this.ready)) return;
    this.#generation += 1;
    this.#load = null;
    this.syncing = false;
    this.dismissHint();
    if (scope !== this.scope) {
      this.approvals.clear(); this.sessions.clear(); this.#revisions.clear(); this.error = false;
    }
    this.scope = scope;
    this.ready = ready;
    if (scope && ready) void this.refresh();
  }

  ingest(envelope: RuntimeEventEnvelope): void {
    if (this.#disposed || !this.scope) return;
    const event = envelope.event;
    let item: PendingItem | undefined;
    if (event.type === 'session_changed' || event.type === 'run_finished') this.#catalog_revision += 1;
    if (event.type === 'session_created') {
      this.sessions.set(event.session.session_id, event.session.title);
      this.#catalog_revision += 1;
    } else if (event.type === 'session_deleted') {
      this.sessions.delete(event.session_id);
      for (const [id, approval] of this.approvals) if (approval.session_id === event.session_id) this.approvals.delete(id);
      this.#catalog_revision += 1;
      this.#bump(event.session_id);
    } else if (event.type === 'approval_requested') {
      this.#bump(event.approval.session_id);
      const existed = this.approvals.has(event.approval.approval_id);
      this.approvals.set(event.approval.approval_id, event.approval);
      if (!existed) item = this.items.find((entry) => entry.target.kind === 'approval' && entry.target.approval_id === event.approval.approval_id);
    } else if (event.type === 'approval_resolved' || event.type === 'approval_cancelled') {
      this.#bump(event.session_id);
      this.approvals.delete(event.approval_id);
    } else if (event.type === 'feedback_published' || event.type === 'feedback_changed') {
      this.#bump(event.request.session_id);
      if (!this.sessions.has(event.request.session_id)) this.sessions.set(event.request.session_id, '其他会话');
      if (event.type === 'feedback_published' && event.request.status === 'pending') {
        item = this.items.find((entry) => entry.target.kind === 'feedback' && entry.target.request_id === event.request.request_id);
      }
    }
    if (this.hint && !this.items.some((entry) => notificationKey(entry.target) === notificationKey(this.hint!.target))) this.dismissHint();
    if (item && this.ready && item.target.owner.session_id !== this.dependencies.selectedSession()) {
      this.dismissHint();
      this.hint = item;
      this.#timer = setTimeout(() => this.dismissHint(), 8000);
    }
  }

  dismissHint(): void {
    if (this.#timer !== null) clearTimeout(this.#timer);
    this.#timer = null;
    this.hint = null;
  }

  refresh(): Promise<void> {
    if (this.#load) return this.#load;
    const client = this.dependencies.client();
    if (!client || !this.scope || !this.ready || this.#disposed) return Promise.resolve();
    const generation = this.#generation;
    const current = () => !this.#disposed && generation === this.#generation && client === this.dependencies.client();
    runInAction(() => { this.syncing = true; });
    const load = this.#synchronize(client, current).then(() => {
      if (current()) runInAction(() => { this.error = false; });
    }).catch(() => {
      if (current()) runInAction(() => { this.error = true; });
    }).finally(() => {
      if (this.#load === load) {
        this.#load = null;
        runInAction(() => { this.syncing = false; });
      }
    });
    this.#load = load;
    return load;
  }

  async #synchronize(client: Pick<RuntimeClient, 'command'>, current: () => boolean): Promise<void> {
    // list_sessions 是分页接口，归档会话也可能仍有待回答事项。
    for (let attempt = 0; attempt < 3 && current(); attempt += 1) {
      const revision = this.#catalog_revision;
      const sessions = new Map<string, string>();
      let offset = 0;
      while (current()) {
        const result = await client.command({ type: 'list_sessions', payload: { filter: 'all', offset, limit: 200, query: null } });
        if (!current()) return;
        for (const session of result.payload.sessions) sessions.set(session.session_id, session.title);
        if (!result.payload.has_more) break;
        if (!result.payload.sessions.length) throw new Error('会话分页未前进');
        offset += result.payload.sessions.length;
      }
      const queue = [...sessions.keys()];
      // 每个 worker 顺序补齐审批和反馈，最多四个读取同时在途。
      const results = await Promise.allSettled(Array.from({ length: Math.min(4, queue.length) }, async () => {
        while (queue.length && current()) {
          const session_id = queue.shift()!;
          await this.#readSession(client, session_id, current);
        }
      }));
      if (!current()) return;
      if (results.some((result) => result.status === 'rejected')) throw new Error('待办尚未同步');
      if (revision !== this.#catalog_revision) continue;
      runInAction(() => {
        this.sessions.replace(sessions);
        for (const [id, approval] of this.approvals) if (!sessions.has(approval.session_id)) this.approvals.delete(id);
        if (this.hint && !this.items.some((entry) => notificationKey(entry.target) === notificationKey(this.hint!.target))) this.dismissHint();
      });
      return;
    }
    if (current()) throw new Error('会话列表变化，请重试同步');
  }

  async #readSession(client: Pick<RuntimeClient, 'command'>, session_id: string, current: () => boolean): Promise<void> {
    for (let attempt = 0; attempt < 3 && current(); attempt += 1) {
      const revision = this.#revisions.get(session_id) ?? 0;
      const result = await client.command({ type: 'list_pending_approvals', payload: { session_id } });
      if (!current()) return;
      await this.dependencies.feedback.refresh(session_id);
      if (!current()) return;
      if (revision !== (this.#revisions.get(session_id) ?? 0)) continue;
      if (this.dependencies.feedback.errors.has(session_id)) throw new Error('问题尚未同步');
      runInAction(() => {
        for (const [id, approval] of this.approvals) if (approval.session_id === session_id) this.approvals.delete(id);
        for (const approval of result.payload.approvals) this.approvals.set(approval.approval_id, approval);
      });
      return;
    }
    if (current()) throw new Error('待办变化，请重试同步');
  }

  #bump(session_id: string): void {
    this.#revisions.set(session_id, (this.#revisions.get(session_id) ?? 0) + 1);
  }

  dispose(): void {
    this.#disposed = true; this.#generation += 1; this.dismissHint();
    this.approvals.clear(); this.sessions.clear();
  }
}
