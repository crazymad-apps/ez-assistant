import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type {} from 'vitest/jsdom';
import type { RuntimeEventEnvelope } from '@ez-assistant/protocol';
import { NotificationStore } from '../../src/features/notifications/NotificationStore';
import {
  notificationFromEvent,
  type NotificationPlatform,
  type NotificationTarget,
} from '../../src/features/notifications/types';
import { deliverOnce } from '../../src/features/notifications/delivery';

const owner = { type: 'main_session', session_id: 'session-1' } as const;
const target: NotificationTarget = { scope: 'host-user-1', owner, run_id: 'run-1', kind: 'turn' };
const notification = { target, title: '任务已结束', body: '点击查看' };
const stores: NotificationStore[] = [];

function fixture() {
  let scope: string | null = target.scope;
  const close = vi.fn();
  const platform: NotificationPlatform = {
    desktop: true,
    permission: vi.fn(async () => 'granted' as const),
    isFocused: vi.fn(async () => false),
    show: vi.fn(async () => close),
    focus: vi.fn(async () => {}),
  };
  const navigate = vi.fn(async (): Promise<boolean | void> => {});
  const store = new NotificationStore({ scope: () => scope, navigate }, platform);
  stores.push(store);
  store.setEnabled(true);
  return {
    store,
    platform,
    navigate,
    close,
    changeScope: (value: string | null) => {
      scope = value;
    },
  };
}

function finished(sequence = 1): RuntimeEventEnvelope {
  return {
    sequence,
    emitted_at_ms: Date.now(),
    event: { type: 'run_finished', session_id: owner.session_id, run_id: 'run-1', status: 'completed', error: null },
  };
}

beforeEach(() => {
  // Node 26 的全局 Web Storage 不属于 jsdom 页面；显式使用浏览器存储。
  vi.stubGlobal('localStorage', jsdom.window.localStorage);
  localStorage.clear();
  vi.stubGlobal('crypto', { randomUUID: () => 'test-id', subtle: { digest: async () => new Uint8Array(32).buffer } });
});
afterEach(() => {
  for (const store of stores.splice(0)) store.dispose();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('系统通知投递与业务隔离', () => {
  it.each(['turn', 'approval', 'feedback'] as const)('窗口聚焦时不发送 %s 通知，失焦后不补发旧事项', async (kind) => {
    const { store, platform } = fixture();
    vi.mocked(platform.isFocused).mockResolvedValue(true);
    let focused_target: NotificationTarget = target;
    if (kind === 'approval') focused_target = { ...target, kind, approval_id: 'approval-1' };
    if (kind === 'feedback') focused_target = { ...target, kind, request_id: 'question-1' };
    await store.publish({ ...notification, target: focused_target });
    expect(platform.show).not.toHaveBeenCalled();
    vi.mocked(platform.isFocused).mockResolvedValue(false);
    await store.publish({ ...notification, target: focused_target });
    expect(platform.show).not.toHaveBeenCalled();
    await store.publish({ ...notification, target: { ...target, run_id: 'new-run' } });
    expect(platform.show).toHaveBeenCalledOnce();
  });

  it('权限等待期间重新聚焦窗口时不显示迟到通知', async () => {
    const { store, platform } = fixture();
    let grant!: (value: 'granted') => void;
    vi.mocked(platform.permission).mockReturnValueOnce(new Promise((resolve) => { grant = resolve; }));
    const pending = store.publish(notification);
    vi.mocked(platform.isFocused).mockResolvedValue(true);
    grant('granted');
    await pending;
    expect(platform.show).not.toHaveBeenCalled();
  });

  it('前台测试只显示页面提示，延迟测试失焦后才发送', async () => {
    vi.useFakeTimers();
    const { store, platform } = fixture();
    vi.mocked(platform.isFocused).mockResolvedValue(true);
    await store.test();
    expect(platform.show).not.toHaveBeenCalled();
    expect(store.notice_message).toContain('处于前台');
    const delayed = store.test(true);
    vi.mocked(platform.isFocused).mockResolvedValue(false);
    await vi.advanceTimersByTimeAsync(10000);
    await delayed;
    expect(platform.show).toHaveBeenCalledOnce();
  });

  it('全局消费完成事件一次，不依赖远端时钟，关闭期间的事件不补发', async () => {
    const { store, platform } = fixture();
    store.ingest({ ...finished(), emitted_at_ms: Date.now() - 60_000 });
    store.ingest(finished());
    store.ingest(finished(2));
    await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(1));
    store.setEnabled(false);
    await store.publish({ ...notification, target: { ...target, run_id: 'run-2' } });
    store.setEnabled(true);
    await store.publish({ ...notification, target: { ...target, run_id: 'run-2' } });
    expect(platform.show).toHaveBeenCalledTimes(1);
    expect(
      notificationFromEvent(target.scope, {
        ...finished(),
        event: {
          ...finished().event,
          type: 'run_finished',
          session_id: 's',
          run_id: 'r',
          status: 'cancelled',
          error: null,
        },
      }),
    ).toBeNull();
  });

  it('权限只在启用按钮请求，拒绝后投递不会再次请求', async () => {
    const { store, platform } = fixture();
    vi.mocked(platform.permission).mockResolvedValue('denied');
    await store.enable();
    expect(platform.permission).toHaveBeenCalledWith(true);
    expect(store.enabled).toBe(false);
    await store.publish(notification);
    expect(platform.permission).toHaveBeenCalledTimes(1);
    expect(platform.show).not.toHaveBeenCalled();
  });

  it('发送异常不传播到业务事件消费，保留可读错误', async () => {
    const { store, platform } = fixture();
    vi.mocked(platform.show).mockRejectedValue(new Error('blocked'));
    await expect(store.publish(notification)).resolves.toBeUndefined();
    expect(store.error_message).toBe('blocked');
  });

  it('点击重新导航，切换账号或释放 Store 后旧通知不能打开', async () => {
    const { store, platform, navigate, changeScope, close } = fixture();
    await store.publish(notification);
    const click = vi.mocked(platform.show).mock.calls[0]![3];
    click();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith(target));
    changeScope('another-user');
    click();
    expect(navigate).toHaveBeenCalledTimes(1);
    store.dispose();
    click();
    expect(navigate).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it('导航因上下文变化取消时不保留过期焦点', async () => {
    const { store, platform, navigate } = fixture();
    navigate.mockResolvedValue(false);
    await store.publish(notification);
    vi.mocked(platform.show).mock.calls[0]![3]();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledWith(target));
    expect(store.focus_target).toBeNull();
    expect(store.error_message).toBeNull();
  });

  it('窗口聚焦迟到时不打开较早点击的事项', async () => {
    const { store, platform, navigate } = fixture();
    await store.publish(notification);
    const last_target: NotificationTarget = { ...target, run_id: 'last-run' };
    await store.publish({ ...notification, target: last_target });
    let focused!: () => void;
    vi.mocked(platform.focus).mockReturnValueOnce(new Promise<void>((resolve) => { focused = resolve; }));
    vi.mocked(platform.show).mock.calls[0]![3]();
    vi.mocked(platform.show).mock.calls[1]![3]();
    await vi.waitFor(() => expect(store.focus_target).toEqual(last_target));
    focused();
    await Promise.resolve();
    expect(navigate).toHaveBeenCalledExactlyOnceWith(last_target);
    expect(store.focus_target).toEqual(last_target);
  });

  it('较早事项的异步定位不能覆盖最新焦点', async () => {
    const { store, platform, navigate } = fixture();
    await store.publish(notification);
    const last_target: NotificationTarget = { ...target, run_id: 'last-run' };
    await store.publish({ ...notification, target: last_target });
    let finish!: () => void;
    navigate.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; }));
    vi.mocked(platform.show).mock.calls[0]![3]();
    await vi.waitFor(() => expect(navigate).toHaveBeenCalledTimes(1));
    vi.mocked(platform.show).mock.calls[1]![3]();
    await vi.waitFor(() => expect(store.focus_target).toEqual(last_target));
    finish();
    await Promise.resolve();
    expect(store.focus_target).toEqual(last_target);
  });

  it('发送在途时关闭通知，迟到句柄会被回收', async () => {
    const { store, platform, close } = fixture();
    let submit!: (value: () => void) => void;
    vi.mocked(platform.show).mockReturnValue(
      new Promise((resolve) => {
        submit = resolve;
      }),
    );
    const pending = store.publish(notification);
    await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(1));
    store.setEnabled(false);
    submit(close);
    await pending;
    expect(close).toHaveBeenCalledOnce();
  });

  it('审批在权限查询期间已解决，不再显示迟到通知', async () => {
    const { store, platform } = fixture();
    let permission!: (value: 'granted') => void;
    vi.mocked(platform.permission).mockReturnValue(
      new Promise((resolve) => {
        permission = resolve;
      }),
    );
    const approval = { ...target, kind: 'approval' as const, approval_id: 'approval-1' };
    const pending = store.publish({ ...notification, target: approval });
    store.ingest({
      sequence: 2,
      emitted_at_ms: Date.now(),
      event: {
        type: 'approval_resolved',
        session_id: owner.session_id,
        run_id: 'run-1',
        approval_id: 'approval-1',
        decision: 'deny',
      },
    });
    permission('granted');
    await pending;
    expect(platform.show).not.toHaveBeenCalled();
  });

  it('F04 一组问题一次提醒，结束轮次不叠加完成通知，旧通知回调重新核验', async () => {
    const { store, platform, navigate, close } = fixture();
    const feedback = { ...target, kind: 'feedback' as const, request_id: 'request-1' };
    const open = vi.fn(async () => {});
    store.registerFeedbackNavigator(open);
    store.feedbackPublished(feedback);
    store.feedbackPublished(feedback);
    store.ingest(finished());
    await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(1));
    vi.mocked(platform.show).mock.calls[0]![3]();
    await vi.waitFor(() => expect(open).toHaveBeenCalledWith(feedback));
    expect(navigate).not.toHaveBeenCalled();
    store.resolved(feedback);
    expect(close).toHaveBeenCalledOnce();
  });

  it('两个 Host 的同名事项不互相去重', async () => {
    const { store, platform, changeScope } = fixture();
    await store.publish(notification);
    changeScope('host-2');
    await store.publish({ ...notification, target: { ...target, scope: 'host-2' } });
    expect(platform.show).toHaveBeenCalledTimes(2);
  });

  it('延迟测试支持切换窗口，等待期间重复点击不会重复发送', async () => {
    vi.useFakeTimers();
    const { store, platform } = fixture();
    const pending = store.test(true);
    await store.test(true);
    expect(store.notice_message).toContain('5 秒');
    await vi.advanceTimersByTimeAsync(4999);
    expect(platform.show).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(platform.show).toHaveBeenCalledOnce();
    expect(store.busy).toBe(true);
    await store.test();
    expect(platform.show).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(5000);
    await pending;
    expect(platform.show).toHaveBeenCalledOnce();
    expect(store.busy).toBe(false);
  });

  it.each(['disable', 'dispose'])('延迟测试在 %s 时取消并释放计时器', async (action) => {
    vi.useFakeTimers();
    const { store, platform } = fixture();
    const clear_timer = vi.spyOn(window, 'clearTimeout');
    const pending = store.test(true);
    if (action === 'disable') store.setEnabled(false);
    else store.dispose();
    await pending;
    expect(clear_timer).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(5000);
    expect(platform.show).not.toHaveBeenCalled();
  });

  it('测试通知不宣称用户已收到，关闭开关会撤回', async () => {
    vi.useFakeTimers();
    const { store, platform, close } = fixture();
    const pending = store.test();
    await vi.advanceTimersByTimeAsync(0);
    expect(store.notice_message).toContain('已提交');
    expect(store.busy).toBe(true);
    await store.test();
    expect(platform.show).toHaveBeenCalledOnce();
    store.setEnabled(false);
    await pending;
    expect(store.busy).toBe(false);
    expect(close).toHaveBeenCalledOnce();
  });
});

describe('浏览器多标签页协调', () => {
  it('串行锁下同一事项仅投递一次，存储只有摘要哈希', async () => {
    let tail = Promise.resolve();
    Object.defineProperty(navigator, 'locks', {
      configurable: true,
      value: {
        request: vi.fn((_name, callback) => {
          const next = tail.then(callback);
          tail = next;
          return next;
        }),
      },
    });
    const send = vi.fn(async () => {});
    await Promise.all([
      deliverOnce('sensitive-session', () => true, send),
      deliverOnce('sensitive-session', () => true, send),
    ]);
    expect(send).toHaveBeenCalledOnce();
    expect(localStorage.getItem('ez-assistant:notification-receipts')).not.toContain('sensitive-session');
    await deliverOnce('other', () => false, send);
    expect(send).toHaveBeenCalledOnce();
  });
});

it('F04 轮后只提醒待反馈，轮内回答后仍可收到完成通知；重连只见终态也不误报完成', async () => {
  const { store, platform } = fixture();
  const request = { request_id: 'question-1', session_id: owner.session_id, run_id: 'run-1', tool_call_id: 'call-1', mode: 'end_turn' as const, status: 'pending' as const, answers: null, questions: [{ question: '审阅反馈？', header: null, options: [], multiple: false }] };
  store.ingest({ sequence: 1, emitted_at_ms: 1, event: { type: 'feedback_published', request } });
  store.ingest({ sequence: 2, emitted_at_ms: 1, event: { type: 'feedback_published', request } });
  await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(1));
  store.ingest(finished(3));
  const end = finished(4);
  if (end.event.type === 'run_finished') store.ingest({ ...end, event: { ...end.event, run_id: 'missed-publish', awaiting_feedback: true } });
  await Promise.resolve();
  expect(platform.show).toHaveBeenCalledTimes(1);
  store.ingest({ sequence: 5, emitted_at_ms: 1, event: { type: 'feedback_published', request: { ...request, mode: 'wait', run_id: 'run-wait', request_id: 'question-wait' } } });
  await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(2));
  if (end.event.type === 'run_finished') store.ingest({ ...end, sequence: 6, event: { ...end.event, run_id: 'run-wait' } });
  await vi.waitFor(() => expect(platform.show).toHaveBeenCalledTimes(3));
});
