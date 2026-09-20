import { beforeEach, describe, expect, it, vi } from 'vitest';
import { configure } from 'mobx';
import { AdminStore } from '../../src/stores/AdminStore';
import { api } from '../../src/request/api';
import { userView } from '../../src/model';
import type { UserRecord, LoginResponse, IdentityResponse } from '../../src/request/openapi';

vi.mock('../../src/request/api', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/request/api')>()),
  api: {
    login: vi.fn(),
    logout: vi.fn(),
    me: vi.fn(),
    users: vi.fn(),
    audit: vi.fn(),
    create: vi.fn(),
    update: vi.fn(),
    reset: vi.fn(),
    password: vi.fn(),
  },
}));
configure({ enforceActions: 'always' });
const user: UserRecord = {
  id: 1,
  username: 'admin',
  display_name: '管理员',
  role: 'admin',
  enabled: true,
  is_super_admin: true,
  created_at: '2026-09-18T00:00:00Z',
  updated_at: '2026-09-18T00:00:00Z',
};
const login: LoginResponse = { center_id: 'center-id', user, token: 'token-a', llm_key: 'never-retain-this-key' };
const draft = { username: 'new-user', displayName: '新用户', role: 'user' as const, password: 'test-password' };
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function signedIn() {
  const store = new AdminStore();
  expect(await store.login('admin', 'password')).toBeUndefined();
  return store;
}
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.login).mockResolvedValue(login);
  vi.mocked(api.logout).mockResolvedValue();
  vi.mocked(api.create).mockResolvedValue(user);
  vi.mocked(api.update).mockResolvedValue(user);
  vi.mocked(api.reset).mockResolvedValue();
  vi.mocked(api.password).mockResolvedValue();
});

describe('身份与凭证隔离', () => {
  it('普通用户误入只撤销新签发 Token，不展示后台', async () => {
    vi.mocked(api.login).mockResolvedValue({ ...login, user: { ...user, role: 'user', is_super_admin: false } });
    const store = new AdminStore();
    expect(await store.login('ordinary', 'password')).toContain('Runtime');
    expect(api.logout).toHaveBeenCalledWith('token-a');
    expect(store.user).toBeUndefined();
  });
  it('超级管理员只认标记，不绑定角色或账号', async () => {
    vi.mocked(api.login).mockResolvedValue({ ...login, user: { ...user, username: 'another', role: 'user' } });
    expect((await signedIn()).user?.username).toBe('another');
  });
  it('重复登录不重复发请求；卸载后的登录响应不会恢复身份', async () => {
    const request = deferred<LoginResponse>();
    vi.mocked(api.login).mockReturnValue(request.promise);
    const store = new AdminStore();
    const pending = store.login('admin', 'password');
    expect(await store.login('admin', 'password')).toContain('正在登录');
    store.dispose();
    request.resolve(login);
    await pending;
    expect(store.token).toBe('');
    expect(api.login).toHaveBeenCalledTimes(1);
    expect(api.logout).toHaveBeenCalledWith(login.token);
  });
});

describe('写入闭环', () => {
  it('成功写入不重复发请求；失败写入返回结果不明且不遗留写入锁', async () => {
    const store = await signedIn();
    expect(await store.saveUser(draft)).toBeUndefined();
    expect(api.create).toHaveBeenCalledTimes(1);
    vi.mocked(api.create).mockRejectedValue(new Error('timeout'));
    expect(await store.saveUser(draft)).toContain('结果未确认');
    expect(store.busy).toBe(false);
    expect(api.create).toHaveBeenCalledTimes(2);
  });
  it('正在提交时阻止重复写入与退出', async () => {
    const store = await signedIn(),
      request = deferred<UserRecord>();
    vi.mocked(api.create).mockReturnValue(request.promise);
    const pending = store.saveUser(draft);
    expect(await store.saveUser(draft)).toContain('正在提交');
    await store.logout();
    expect(api.logout).not.toHaveBeenCalled();
    request.resolve(user);
    await pending;
    expect(api.create).toHaveBeenCalledTimes(1);
  });
  it('管理重置他人不退出自己、不发送原密码', async () => {
    const store = await signedIn();
    await store.resetPassword(userView({ ...user, id: 2 }), 'new-password');
    expect(api.reset).toHaveBeenCalledWith(login.token, 2, 'new-password');
    expect(api.password).not.toHaveBeenCalled();
    expect(store.token).toBe(login.token);
  });
  it('管理重置自己清除登录，仍走管理接口', async () => {
    const store = await signedIn();
    await store.resetPassword(userView(user), 'new-password');
    expect(store.token).toBe('');
    expect(api.reset).toHaveBeenCalledWith(login.token, user.id, 'new-password');
    expect(api.password).not.toHaveBeenCalled();
  });
  it('本人改密传原密码与新密码，成功清除登录', async () => {
    const store = await signedIn();
    await store.changePassword('old-password', 'new-password');
    expect(store.token).toBe('');
    expect(api.password).toHaveBeenCalledWith(login.token, 'old-password', 'new-password');
    expect(api.reset).not.toHaveBeenCalled();
  });
  it('修改本人名称同步顶栏身份；不错误退出', async () => {
    const store = await signedIn();
    vi.mocked(api.update).mockResolvedValue({ ...user, display_name: '新名称' });
    await store.saveUser({ ...draft, role: 'admin' }, userView(user));
    expect(store.currentUser?.displayName).toBe('新名称');
    expect(store.token).toBe(login.token);
  });
  it('修改前发起的本人查询不能覆盖已保存的新名称', async () => {
    const store = await signedIn(),
      old = deferred<IdentityResponse>();
    vi.mocked(api.me).mockReturnValue(old.promise);
    const pending = store.verifyIdentity();
    vi.mocked(api.update).mockResolvedValue({ ...user, display_name: '最新名称' });
    await store.saveUser({ ...draft, role: 'admin' }, userView(user));
    old.resolve({ center_id: login.center_id, user });
    await pending;
    expect(store.currentUser?.displayName).toBe('最新名称');
  });
});
