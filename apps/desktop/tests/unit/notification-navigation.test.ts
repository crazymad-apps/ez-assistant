import { afterEach, describe, expect, it, vi } from 'vitest';
import { runInAction } from 'mobx';
import { MIN_COMPATIBLE_VERSION, SOFTWARE_VERSION, type SessionViewSnapshot } from '@ez-assistant/protocol';
import type { NotificationTarget } from '../../src/features/notifications';
import { RuntimeClient } from '../../src/runtime-client/RuntimeClient';
import { RootStore } from '../../src/stores/RootStore';
import { RuntimeLifecycleCoordinator } from '../../src/stores/RuntimeLifecycleCoordinator';

const stores: RootStore[] = [];
const clients: RuntimeClient[] = [];
afterEach(() => {
  for (const store of stores.splice(0)) store.dispose();
  for (const client of clients.splice(0)) client.dispose();
  vi.restoreAllMocks();
});

function fixture() {
  const client = new RuntimeClient({
    base_url: 'http://127.0.0.1:9000', access_token: 'fixture', instance_id: 'one', started_runtime: false,
    capabilities: { mode: 'personal', min_compatible_version: MIN_COMPATIBLE_VERSION, runtime_version: SOFTWARE_VERSION,
      max_command_bytes: 65536, max_attachment_bytes: null, sse: true, streaming_upload: true, features: [] },
  });
  clients.push(client);
  const current_client = vi.spyOn(RuntimeLifecycleCoordinator.prototype, 'client', 'get').mockReturnValue(client);
  const load_child = vi.spyOn(RuntimeLifecycleCoordinator.prototype, 'loadChildTask').mockResolvedValue();
  const command = vi.spyOn(client, 'command').mockResolvedValue(response('destination'));
  const store = new RootStore();
  stores.push(store);
  store.navigation.selectSession('previous');
  const target: NotificationTarget = {
    scope: store.notificationScope()!, owner: { type: 'main_session', session_id: 'destination' },
    run_id: 'run-1', kind: 'turn',
  };
  return { store, command, current_client, load_child, target };
}

function response(session_id: string, lifecycle: 'active' | 'archived' = 'active') {
  const value: SessionViewSnapshot = {
    session: {
      session_id, title: session_id, model_selection: null, lifecycle, role: 'standard', current_variant: 'build',
      approval_mode: 'ask', workspace_id: null, active_run_id: null, message_count: 0, queued_input_count: 0,
      resume_required: false, created_at_ms: 1, updated_at_ms: 1, archived_at_ms: lifecycle === 'archived' ? 2 : null,
      is_pinned: false, title_origin: 'user', pending_approval_count: 0, active_child_count: 0, active_run_status: null,
    },
    conversation_generation: 1,
    composer_capabilities: { model_error: null, selected_model: null, reasoning_effort_options: [], image_handling: 'unavailable', goal_supported: false },
    work_plan: null, goal: null, active_run: null, queue: { revision: 1, state: 'automatic', items: [] },
    approvals: { revision: 1, items: [], resolving_approval_id: null },
    attachments: [], file_references: [], artifacts: [], runs: [], child_tasks: [], active_skills: [],
    usage: { accumulated: null, previous_turn: null, latest_cache_hit_basis_points: null, overall_cache_hit_basis_points: null, context: null },
    conversation: { owner: { type: 'main_session', session_id }, generation: 1, items: [], previous_cursor: null, has_more: false },
  };
  return { type: 'get_session_view' as const, payload: { snapshot: { observed_sequence: 1, value } } };
}

describe('通知返回业务位置', () => {
  it.each(['active', 'archived'] as const)('重读 %s 会话、关闭设置且清除旧定位', async (lifecycle) => {
    const { store, command, target } = fixture();
    command.mockResolvedValue(response('destination', lifecycle));
    runInAction(() => { store.settings.is_open = true; });
    const clear_focus = vi.spyOn(store.transient_focus, 'clear');
    expect(await store.openNotificationTarget(target)).toBe(true);
    expect(command).toHaveBeenCalledExactlyOnceWith({ type: 'get_session_view', payload: { session_id: 'destination' } });
    expect(store.navigation.selected_session_id).toBe('destination');
    expect(store.navigation.list_mode).toBe(lifecycle);
    expect(store.settings.is_open).toBe(false);
    expect(clear_focus).toHaveBeenCalledOnce();
  });

  it('子任务审批回到父会话审批区，过期审批只提示而不执行决定', async () => {
    const { store, command, load_child, target } = fixture();
    expect(await store.openNotificationTarget({ ...target, kind: 'approval', approval_id: 'expired',
      owner: { type: 'child_task', session_id: 'destination', child_task_id: 'child-1' } })).toBe(true);
    expect(store.navigation.selected_session_id).toBe('destination');
    expect(store.navigation.selected_child_task_id).toBeNull();
    expect(load_child).not.toHaveBeenCalled();
    expect(store.interaction_error).toContain('已处理或已失效');
    expect(command).toHaveBeenCalledTimes(1);
    await store.openNotificationTarget(target);
    expect(store.interaction_error).toBeNull();
  });

  it('子任务内容目标打开对应子任务', async () => {
    const { store, load_child, target } = fixture();
    expect(await store.openNotificationTarget({ ...target,
      owner: { type: 'child_task', session_id: 'destination', child_task_id: 'child-1' } })).toBe(true);
    expect(store.navigation.selected_child_task_id).toBe('child-1');
    expect(load_child).toHaveBeenCalledExactlyOnceWith('destination', 'child-1');
  });

  it.each(['selection', 'identity', 'dispose'] as const)('读取期间 %s 改变时，迟到响应不抢占导航', async (change) => {
    const { store, command, current_client, target } = fixture();
    let finish!: (value: ReturnType<typeof response>) => void;
    command.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const opening = store.openNotificationTarget(target);
    if (change === 'selection') store.navigation.selectSession('user-chosen');
    if (change === 'identity') current_client.mockReturnValue(null);
    if (change === 'dispose') store.dispose();
    finish(response('destination'));
    expect(await opening).toBe(false);
    expect(store.navigation.selected_session_id).toBe(change === 'selection' ? 'user-chosen' : 'previous');
    expect(store.projection.session_views.has('destination')).toBe(false);
  });

  it('连续点击时以最后一条为准，较早响应不能覆盖', async () => {
    const { store, command, target } = fixture();
    let finish!: (value: ReturnType<typeof response>) => void;
    command.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const first = store.openNotificationTarget(target);
    command.mockResolvedValueOnce(response('last'));
    expect(await store.openNotificationTarget({ ...target, owner: { type: 'main_session', session_id: 'last' } })).toBe(true);
    finish(response('destination'));
    expect(await first).toBe(false);
    expect(store.navigation.selected_session_id).toBe('last');
  });

  it('拒绝其他登录域与错误会话响应，失败时保留原页面', async () => {
    const { store, command, target } = fixture();
    expect(await store.openNotificationTarget({ ...target, scope: 'other-login' })).toBe(false);
    expect(command).not.toHaveBeenCalled();
    command.mockResolvedValue(response('wrong-session'));
    await expect(store.openNotificationTarget(target)).rejects.toThrow('通知来源会话不匹配');
    expect(store.navigation.selected_session_id).toBe('previous');
    command.mockRejectedValue(new Error('session unavailable'));
    await expect(store.openNotificationTarget(target)).rejects.toThrow('session unavailable');
    expect(store.navigation.selected_session_id).toBe('previous');
  });
});


it('reopens a released device for its notification without querying the local session with the same id', async () => {
  const { store, command, target } = fixture();
  const device = '11111111-1111-4111-8111-111111111111';
  const local = store.local_runtime_client!;
  const remote = new RuntimeClient({ base_url: local.address, access_token: 'fixture', instance_id: local.instance_id,
    capabilities: local.capabilities, started_runtime: false }, undefined, device);
  clients.push(remote);
  const remote_command = vi.spyOn(remote, 'command').mockResolvedValue(response('destination'));
  runInAction(() => { store.network.devices = [{ id: device, name: 'remote', platform: 'test', enabled: true, online: true, desktop_view: false, desktop_control: false }]; });
  const reconnect = vi.spyOn(store.network, 'ensureConnected').mockImplementation(async (id) => {
    vi.spyOn(store.network.context(id).runtime, 'client', 'get').mockReturnValue(remote);
  });
  expect(await store.openNotificationTarget({ ...target, scope: store.notificationScopeFor(remote)! })).toBe(true);
  expect(reconnect).toHaveBeenCalledExactlyOnceWith(device);
  expect(remote_command).toHaveBeenCalledExactlyOnceWith({ type: 'get_session_view', payload: { session_id: 'destination' } });
  expect(command).not.toHaveBeenCalled();
  expect(store.navigation.target_device).toBe(device);
  expect(store.local_projection.session_views.has('destination')).toBe(false);
});
