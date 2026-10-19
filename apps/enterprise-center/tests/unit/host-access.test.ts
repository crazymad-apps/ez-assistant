import { randomUUID } from 'node:crypto';
import type { EntityManager } from 'typeorm';
import { afterEach, expect, it, vi } from 'vitest';
import { NetworkAccess } from '../../src/host-devices/access.js';
import { IdentityService } from '../../src/identity/service.js';
import type { LoginResponse } from '../../src/contracts/identity.js';
import type { HostDevicesService } from '../../src/host-devices/service.js';
import type { HostDeviceDetail } from '../../src/contracts/host-devices.js';
import type { DeviceConnection } from '../../src/host-devices/connection.js';

const cleanup: (() => void)[] = [];
afterEach(() => { cleanup.splice(0).forEach((close) => close()); vi.restoreAllMocks(); vi.useRealTimers(); });
function fixture() {
  const identity = new IdentityService();
  const user = (id: number) => ({ id, username: `user${id}`, display_name: `User ${id}`, role: 'user' as const, enabled: true, is_super_admin: false });
  const login = (token: string, id = 1): LoginResponse => ({ center_id: 'center', user: user(id), token, llm_key: `key-${token}` });
  for (const token of ['source1', 'source2', 'target']) identity.publishLogin(login(token));
  vi.spyOn(identity, 'requireUser').mockImplementation(async (_, token) => user(identity.scope(token!).userId));
  let queue = Promise.resolve();
  vi.spyOn(identity, 'write').mockImplementation((work, committed = () => {}) => {
    const next = queue.then(async () => { const result = await work({} as EntityManager); committed(result); return result; });
    queue = next.then(() => {}, () => {}); return next;
  });
  const allow = vi.fn().mockResolvedValue(undefined);
  const devices = { allow } as unknown as HostDevicesService;
  function peer() {
    const sent: Record<string, unknown>[] = [];
    const connection = { connectionId: randomUUID(), online: true, send: (value: Record<string, unknown>, done?: (ok: boolean) => void) => { sent.push(value); done?.(true); return true; } } as unknown as DeviceConnection;
    return { sent, connection };
  }
  const a = peer(), b = peer();
  const device = randomUUID();
  const access = new NetworkAccess(identity, devices, (id) => id === device ? b.connection : undefined);
  cleanup.push(() => access.close());
  const command = (connection: DeviceConnection, action: string, payload: object) => access.dispatch(connection, { type: 'request', request_id: randomUUID(), action, payload });
  async function open(token = 'source1', capability = 'ordinary', view_id?: string) {
    const id = randomUUID();
    const opening = command(a.connection, 'open', { access_id: id, token, device_id: device, capability, ...(view_id ? { view_id } : {}) });
    await vi.waitFor(() => expect(b.sent.some((v) => v.type === 'access_offer' && v.access_id === id)).toBe(true));
    await command(b.connection, 'ready', { access_id: id, target_token: 'target' }); await opening;
    return id;
  }
  return { a, b, access, identity, allow, device, command, open };
}
it('单个 Token 撤销只结束该登录的访问，不影响同用户另一登录', async () => {
  const f = fixture(), first = await f.open(), second = await f.open('source2');
  f.identity.discardLogin('source1');
  expect(f.a.sent.filter((v) => v.type === 'access_revoked').map((v) => v.access_id)).toEqual([first]);
  await f.command(f.b.connection, 'renew', { access_id: second, target_token: 'target' });
  expect(f.b.sent.at(-1)).toMatchObject({ type: 'response', result: { valid_for_ms: 45000 } });
});
it('桌面权限移除只撤销桌面访问，普通访问和目标登录保留', async () => {
  const f = fixture(), ordinary = await f.open(), desktop = await f.open('source1', 'desktop_view');
  f.access.changed(f.device, false, { status: 'registered', enabled: true, users: [{ user_id: 1, desktop_view: false, desktop_control: false }] } as HostDeviceDetail);
  expect(f.a.sent.filter((v) => v.type === 'access_revoked').map((v) => v.access_id)).toEqual([desktop]);
  expect(f.identity.scope('target').userId).toBe(1);
  await f.command(f.b.connection, 'renew', { access_id: ordinary, target_token: 'target' });
  expect(f.b.sent.at(-1)).toHaveProperty('result');
});
it('目标持续续期不能替失联调用端无限续权，旧连接也不能续权', async () => {
  const f = fixture(), id = await f.open();
  // 只移动单调时钟；到期检查同时由每次请求执行，不依赖 interval 恰好已运行。
  let now = performance.now(); vi.spyOn(performance, 'now').mockImplementation(() => now);
  now += 30000;
  await f.command(f.b.connection, 'renew', { access_id: id, target_token: 'target' });
  expect(f.b.sent.at(-1)).toHaveProperty('result');
  now += 16000;
  await f.command(f.b.connection, 'renew', { access_id: id, target_token: 'target' });
  expect(f.b.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_ENDED' });
});
it('只有目标当前连接能兑换登录，发送失败或中途撤销清理临时凭据', async () => {
  const f = fixture();
  const login: LoginResponse = { center_id: 'center', user: { id: 1, username: 'user1', display_name: 'User 1', role: 'user', enabled: true, is_super_admin: false }, token: 'issued', llm_key: 'issued-key' };
  vi.spyOn(f.identity, 'prepareLogin').mockResolvedValue(login);
  const id = randomUUID();
  const opening = f.command(f.a.connection, 'open', { access_id: id, token: 'source1', device_id: f.device, capability: 'ordinary' });
  await vi.waitFor(() => expect(f.b.sent.some((v) => v.type === 'access_offer')).toBe(true));
  await f.command(f.a.connection, 'login', { access_id: id });
  expect(f.a.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
  await f.command(f.b.connection, 'login', { access_id: id });
  expect(f.identity.scope('issued').userId).toBe(1);
  f.access.disconnected(f.a.connection); await opening;
  expect(() => f.identity.scope('issued')).toThrow();
  expect(f.identity.scope('target').userId).toBe(1);
});
it('close 等待此前 open 的身份事务提交，迟到授权不能留下孤立访问', async () => {
  const f = fixture();
  let release!: () => void;
  const gate = new Promise<void>((resolve) => { release = resolve; });
  const blocking = f.identity.write(async () => { await gate; });
  const id = randomUUID();
  const opening = f.command(f.a.connection, 'open', { access_id: id, token: 'source1', device_id: f.device, capability: 'ordinary' });
  const closing = f.command(f.a.connection, 'close', { access_id: id });
  release(); await blocking; await closing; await opening;
  expect(f.b.sent.some((v) => v.type === 'access_revoked' && v.access_id === id)).toBe(true);
  await f.command(f.b.connection, 'ready', { access_id: id, target_token: 'target' });
  expect(f.b.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_ENDED' });
});

it('桌面信令沿已授权来源和目标双向转发，普通访问不能发送', async () => {
  const f = fixture(), id = await f.open('source1', 'desktop_view');
  expect(f.b.sent.find((v) => v.type === 'access_offer')).toMatchObject({ source_connection_id: f.a.connection.connectionId });
  const signal = (connection: DeviceConnection, type: string, data: object) => f.command(connection, 'desktop_signal', { access_id: id, signal: { type, data } });
  await signal(f.a.connection, 'prepare', { fmtp: 'h264' });
  expect(f.b.sent.at(-1)).toMatchObject({ type: 'desktop_signal', access_id: id, signal: { type: 'prepare' } });
  await signal(f.b.connection, 'offer', { type: 'offer', sdp: 'video', desktop: { selected: 'main', displays: [{ id: 'main', width: 1920, height: 1080, primary: true }] } });
  expect(f.a.sent.at(-1)).toMatchObject({ type: 'desktop_signal', signal: { type: 'offer', data: { desktop: { selected: 'main' } } } });
  await signal(f.a.connection, 'answer', { type: 'answer', sdp: 'video' });
  await signal(f.b.connection, 'candidate', { candidate: 'candidate' });
  expect(f.a.sent.at(-1)).toMatchObject({ type: 'desktop_signal', signal: { type: 'candidate' } });
  const ordinary = await f.open();
  await f.command(f.a.connection, 'desktop_signal', { access_id: ordinary, signal: { type: 'prepare', data: {} } });
  expect(f.a.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
});
it('错方向、超限和已撤销桌面信令不能恢复访问', async () => {
  const f = fixture(), id = await f.open('source1', 'desktop_view');
  await f.command(f.b.connection, 'desktop_signal', { access_id: id, signal: { type: 'prepare', data: {} } });
  expect(f.b.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
  const before = f.b.sent.length;
  await f.command(f.a.connection, 'desktop_signal', { access_id: id, signal: { type: 'prepare', data: {} } });
  expect(f.b.sent).toHaveLength(before);
  const next = await f.open('source1', 'desktop_view');
  await f.command(f.a.connection, 'desktop_signal', { access_id: next, signal: { type: 'prepare', data: {} } });
  await f.command(f.b.connection, 'desktop_signal', { access_id: next, signal: { type: 'offer', data: { sdp: 'x'.repeat(8193) } } });
  expect(f.b.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
});

it('控制绑定当前查看、用户和来源，查看关闭级联控制但不影响普通业务', async () => {
  const f = fixture(), ordinary = await f.open(), view = await f.open('source1', 'desktop_view');
  await f.command(f.a.connection, 'open', { access_id: randomUUID(), token: 'source1', device_id: f.device, capability: 'desktop_control' });
  expect(f.a.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
  await f.command(f.a.connection, 'open', { access_id: randomUUID(), token: 'source2', device_id: f.device, capability: 'desktop_control', view_id: view });
  expect(f.a.sent.at(-1)).toMatchObject({ error: 'DEVICE_ACCESS_DENIED' });
  const control = await f.open('source1', 'desktop_control', view);
  expect(f.b.sent.find((v) => v.type === 'access_offer' && v.access_id === control)).toMatchObject({ view_id: view });
  await f.command(f.a.connection, 'close', { access_id: view });
  expect(f.a.sent.filter((v) => v.type === 'access_revoked').map((v) => v.access_id)).toEqual(expect.arrayContaining([view, control]));
  await f.command(f.b.connection, 'renew', { access_id: ordinary, target_token: 'target' });
  expect(f.b.sent.at(-1)).toHaveProperty('result');
});
it('仅撤销控制保留查看连接', async () => {
  const f = fixture(), view = await f.open('source1', 'desktop_view');
  const control = await f.open('source1', 'desktop_control', view);
  f.access.changed(f.device, false, { status: 'registered', enabled: true, users: [{ user_id: 1, desktop_view: true, desktop_control: false }] } as HostDeviceDetail);
  expect(f.a.sent.filter((v) => v.type === 'access_revoked').map((v) => v.access_id)).toEqual([control]);
  await f.command(f.b.connection, 'renew', { access_id: view, target_token: 'target' });
  expect(f.b.sent.at(-1)).toHaveProperty('result');
});
