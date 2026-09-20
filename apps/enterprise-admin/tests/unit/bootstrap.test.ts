import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AxiosError } from 'axios';
import type { InternalAxiosRequestConfig, AxiosResponse } from 'axios';
import { bootstrap } from '../../src/bootstrap';
import { client } from '../../src/request/openapi/client.gen';
import * as sdk from '../../src/request/openapi/sdk.gen';
import { api } from '../../src/request/api';

const user = {
  id: 1,
  username: 'admin',
  display_name: '管理员',
  role: 'admin',
  enabled: true,
  is_super_admin: true,
};
let app: ReturnType<typeof bootstrap>;
let requests: InternalAxiosRequestConfig[];
let token: string;
let respond: (config: InternalAxiosRequestConfig) => Promise<AxiosResponse>;
function response(config: InternalAxiosRequestConfig, data: unknown): AxiosResponse {
  return { config, data, status: 200, statusText: 'OK', headers: {} };
}
function unauthorized(config: InternalAxiosRequestConfig, code = 'TOKEN_INVALID') {
  return new AxiosError('unsafe raw text', 'ERR_BAD_REQUEST', config, undefined, {
    ...response(config, { error: { code, message: 'secret' } }),
    status: 401,
  });
}
beforeEach(() => {
  requests = [];
  token = 'token-a';
  app = bootstrap();
  respond = async (config) => {
    if (config.url === '/api/auth/login')
      return response(config, { center_id: 'center-id', user, token, llm_key: 'unused-key' });
    if (config.url === '/api/auth/me') return response(config, { center_id: 'center-id', user });
    return response(config, {
      items: [{ ...user, created_at: '2026-09-18T00:00:00Z', updated_at: '2026-09-18T00:00:00Z' }],
      total: 1,
    });
  };
  client.setConfig({
    adapter: async (config) => {
      requests.push(config);
      return respond(config);
    },
  });
});
afterEach(() => {
  app.dispose();
  client.setConfig({ adapter: undefined });
  vi.restoreAllMocks();
});

describe('bootstrap 统一请求初始化', () => {
  it('生成 SDK 直接调用自动读取当前 Token，不缓存到 client defaults', async () => {
    await app.store.login('admin', 'password');
    await sdk.getCurrentIdentity();
    expect(requests.at(-1)!.headers.get('Authorization')).toBe('Bearer token-a');
    await app.store.logout();
    await app.store.login('admin', 'password');
    token = 'token-b';
    await app.store.login('admin', 'password');
    await sdk.getCurrentIdentity();
    expect(requests.at(-1)!.headers.get('Authorization')).toBe('Bearer token-b');
    expect(JSON.stringify(client.getConfig())).not.toContain('token-b');
  });
  it('保留显式请求身份，不把误入账号撤销变成当前管理员退出', async () => {
    await app.store.login('admin', 'password');
    await api.logout('other-token');
    expect(requests.at(-1)!.headers.get('Authorization')).toBe('Bearer other-token');
    expect(app.store.token).toBe('token-a');
  });
  it('迟到的旧身份失败不能退出新登录', async () => {
    await app.store.login('admin', 'password');
    const normal = respond;
    let reject!: (error: unknown) => void, entered!: () => void;
    const started = new Promise<void>((resolve) => {
      entered = resolve;
    });
    respond = (config) =>
      config.url === '/api/auth/me'
        ? new Promise((_resolve, no) => {
            reject = () => no(unauthorized(config));
            entered();
          })
        : normal(config);
    const oldRequest = sdk.getCurrentIdentity();
    const rejected = expect(oldRequest).rejects.toBeInstanceOf(AxiosError);
    await started;
    await app.store.logout();
    token = 'token-b';
    await app.store.login('admin', 'password');
    reject(undefined);
    await rejected;
    expect(app.store.token).toBe('token-b');
    expect(app.store.notice).toBe('');
  });
  it('取消的请求、无 response 的断网和普通业务错误均不误退出', async () => {
    await app.store.login('admin', 'password');
    const abort = new AbortController();
    abort.abort();
    await expect(sdk.getCurrentIdentity({ signal: abort.signal })).rejects.toThrow();
    respond = async () => {
      throw new AxiosError('network error');
    };
    await expect(sdk.getCurrentIdentity()).rejects.toThrow('network error');
    respond = async (config) => {
      throw unauthorized(config, 'USERNAME_EXISTS');
    };
    await expect(sdk.getCurrentIdentity()).rejects.toThrow();
    expect(app.store.token).toBe('token-a');
  });
  it('退出响应未确认时保留重试入口，不由失效拦截器假报成功', async () => {
    await app.store.login('admin', 'password');
    respond = async (config) => {
      throw unauthorized(config);
    };
    await app.store.logout();
    expect(app.store.logoutState).toBe('failed');
    expect(app.store.token).toBe('token-a');
  });
});
