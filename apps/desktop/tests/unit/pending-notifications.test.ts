import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ApprovalSnapshot, FeedbackRequestSnapshot, RuntimeEvent, SessionSummary } from '@ez-assistant/protocol';
import type { RuntimeClient } from '../../src/runtime-client/RuntimeClient';
import { PendingNotificationStore } from '../../src/features/notifications/PendingNotificationStore';
import { FeedbackStore } from '../../src/features/feedback/FeedbackStore';

const disposers: (() => void)[] = [];
afterEach(() => { disposers.splice(0).forEach((dispose) => dispose()); vi.useRealTimers(); });
const approval = (id: string, session_id = 'other'): ApprovalSnapshot => ({
  approval_id: id, session_id, run_id: 'run', child_task_id: 'child',
  subject: { type: 'shell', tool_name: 'shell' },
} as ApprovalSnapshot);
const feedback = (id: string, session_id = 'other'): FeedbackRequestSnapshot => ({
  request_id: id, session_id, run_id: 'run', tool_call_id: 'call', mode: 'wait', status: 'pending', answers: null,
  questions: [{ question: '继续使用此方案？', header: null, options: [], multiple: false }],
});
const session = (id: string) => ({ session_id: id, title: `会话 ${id}` }) as SessionSummary;
function fixture(ids = ['current', 'other']) {
  let scope: string | null = 'scope-1';
  let sequence = 0;
  const approvals = new Map<string, ApprovalSnapshot[]>();
  const requests = new Map<string, FeedbackRequestSnapshot[]>();
  const command = vi.fn(async (input: { type: string; payload: { session_id?: string; offset?: number } }): Promise<{ payload: unknown }> => {
    if (input.type === 'list_sessions') return { payload: { sessions: ids.map(session), has_more: false } };
    if (input.type === 'list_pending_approvals') return { payload: { approvals: approvals.get(input.payload.session_id!) ?? [] } };
    if (input.type === 'list_feedback_requests') return { payload: requests.get(input.payload.session_id!) ?? [] };
    throw new Error(input.type);
  });
  const client = { command } as unknown as RuntimeClient;
  const questions = new FeedbackStore({ client: () => client, scope: () => scope });
  questions.setScope(scope);
  const store = new PendingNotificationStore({ client: () => client, selectedSession: () => 'current', title: () => undefined, feedback: questions });
  disposers.push(() => { store.dispose(); questions.dispose(); });
  return { store, questions, approvals, requests, command,
    async start() { store.setConnection(scope, true); await store.refresh(); },
    emit(event: RuntimeEvent) {
      const envelope = { sequence: ++sequence, emitted_at_ms: sequence, event };
      questions.ingest(envelope); store.ingest(envelope);
    },
    setScope(next: string | null) { scope = next; questions.setScope(scope); store.setConnection(scope, false); },
  };
}

describe('跨会话待处理投影', () => {
  it('恢复所有分页（含归档）的审批与问答，无短提示；子审批保留父会话归属', async () => {
    const f = fixture();
    const default_command = f.command.getMockImplementation()!;
    f.command.mockImplementation(async (input) => input.type === 'list_sessions'
      ? { payload: { sessions: [session(input.payload.offset ? 'archived' : 'other')], has_more: !input.payload.offset } }
      : default_command(input));
    f.approvals.set('other', [approval('a')]); f.requests.set('archived', [feedback('q', 'archived')]);
    await f.start();
    expect(f.store.groups.map((group) => [group.title, group.items.length])).toEqual([['会话 other', 1], ['会话 archived', 1]]);
    expect(f.store.items[0]?.target.owner).toEqual({ type: 'child_task', child_task_id: 'child', session_id: 'other' });
    expect(f.store.hint).toBeNull();
    expect(f.command).toHaveBeenCalledWith({ type: 'list_sessions', payload: { filter: 'all', offset: 1, limit: 200, query: null } });
  });

  it('新事件只提示其他会话，关闭或超时不删除待办，处理事件立即撤销', async () => {
    const f = fixture(); await f.start(); vi.useFakeTimers();
    f.emit({ type: 'approval_requested', approval: approval('current-a', 'current') });
    expect(f.store.hint).toBeNull();
    f.emit({ type: 'approval_requested', approval: approval('a') });
    expect(f.store.hint?.target.kind).toBe('approval');
    f.store.dismissHint(); expect(f.store.items).toHaveLength(2);
    f.emit({ type: 'feedback_published', request: feedback('q') });
    expect(f.store.hint?.target.kind).toBe('feedback');
    vi.advanceTimersByTime(8000); expect(f.store.hint).toBeNull(); expect(f.store.items).toHaveLength(3);
    f.emit({ type: 'approval_resolved', session_id: 'other', run_id: 'run', approval_id: 'a', decision: 'allow_once' });
    f.emit({ type: 'feedback_changed', request: { ...feedback('q'), status: 'cancelled' } });
    expect(f.store.items).toHaveLength(1);
  });

  it('gap/重连重新读取权威结果，不补弹旧提示，失败可见且可以重试', async () => {
    const f = fixture(); await f.start();
    f.emit({ type: 'approval_requested', approval: approval('old') });
    f.store.setConnection('scope-1', false);
    f.command.mockRejectedValueOnce(new Error('offline'));
    f.store.setConnection('scope-1', true); await f.store.refresh();
    expect(f.store.error).toBe(true); expect(f.store.items).toHaveLength(1); expect(f.store.hint).toBeNull();
    f.requests.set('other', [feedback('restored')]);
    await f.store.refresh();
    expect(f.store.error).toBe(false); expect(f.store.items).toHaveLength(1);
    expect(f.store.items[0]?.target.kind).toBe('feedback'); expect(f.store.hint).toBeNull();
  });

  it('补读期间已解决的审批不会被迟到快照恢复，重读仍保留其他事项', async () => {
    const f = fixture(); await f.start();
    let release!: (value: { payload: unknown }) => void;
    const default_command = f.command.getMockImplementation()!;
    let delayed = true;
    f.command.mockImplementation((input) => {
      if (input.type === 'list_pending_approvals' && input.payload.session_id === 'other' && delayed) {
        delayed = false; return new Promise((resolve) => { release = resolve; });
      }
      return default_command(input);
    });
    const reading = f.store.refresh();
    await vi.waitFor(() => expect(release).toBeDefined());
    f.emit({ type: 'approval_resolved', session_id: 'other', run_id: 'run', approval_id: 'a', decision: 'deny' });
    f.approvals.set('other', [approval('b')]);
    release({ payload: { approvals: [approval('a'), approval('b')] } });
    await reading;
    expect(f.store.items.map((item) => item.target.kind === 'approval' && item.target.approval_id)).toEqual(['b']);
  });

  it('换身份清除旧列表与提示，丢弃在途结果', async () => {
    const f = fixture(); await f.start();
    f.emit({ type: 'approval_requested', approval: approval('a') });
    let release!: (value: { payload: unknown }) => void;
    f.command.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const loading = f.store.refresh(); f.setScope('scope-2');
    release({ payload: { sessions: [session('other')], has_more: false } }); await loading;
    expect(f.store.items).toHaveLength(0); expect(f.store.hint).toBeNull(); expect(f.store.syncing).toBe(false);
  });

  it('会话删除使在途反馈读取失效', async () => {
    const f = fixture(); await f.start();
    let release!: (value: { payload: unknown }) => void;
    f.command.mockImplementationOnce(() => new Promise((resolve) => { release = resolve; }));
    const loading = f.questions.refresh('other');
    f.emit({ type: 'session_deleted', session_id: 'other' });
    release({ payload: [feedback('q')] }); await loading;
    expect(f.questions.requests.size).toBe(0); expect(f.store.items).toHaveLength(0);
  });

  it('反馈查询失败保留错误，而不是把全局列表误报为空', async () => {
    const f = fixture();
    const default_command = f.command.getMockImplementation()!;
    f.command.mockImplementation(async (input) => {
      if (input.type === 'list_feedback_requests') throw new Error('feedback offline');
      return default_command(input);
    });
    await f.start(); expect(f.store.error).toBe(true);
  });

  it('补查询最多四个并发，单个失败后等待其他在途请求完成再允许重试', async () => {
    const f = fixture(Array.from({ length: 12 }, (_, i) => String(i)));
    const default_command = f.command.getMockImplementation()!;
    let active = 0; let maximum = 0;
    f.command.mockImplementation(async (input) => {
      active += 1; maximum = Math.max(maximum, active);
      await new Promise((resolve) => setTimeout(resolve, 1));
      active -= 1;
      if (input.type === 'list_pending_approvals' && input.payload.session_id === '0') throw new Error('failed');
      return default_command(input);
    });
    await f.start(); expect(maximum).toBeLessThanOrEqual(4); expect(active).toBe(0); expect(f.store.error).toBe(true);
  });
});
