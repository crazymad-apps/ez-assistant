import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AxiosError } from 'axios';
import type { AxiosResponse, InternalAxiosRequestConfig } from 'axios';
import { bootstrap } from '../../src/bootstrap';
import { client } from '../../src/request/openapi/client.gen';
import { clearTokenCookie, readTokenCookie, writeTokenCookie } from '../../src/stores/token-cookie';

const user = {
  id: 1,
  username: 'admin',
  display_name: '管理员',
  role: 'admin',
  enabled: true,
  is_super_admin: true,
};
let app: ReturnType<typeof bootstrap>;
let cookies: Map<string, string>;
let writes: string[];
let requests: InternalAxiosRequestConfig[];
let respond: (config: InternalAxiosRequestConfig) => Promise<AxiosResponse>;

function response(config: InternalAxiosRequestConfig, data: unknown): AxiosResponse {
  return { config, data, status: 200, statusText: 'OK', headers: {} };
}

beforeEach(() => {
  cookies = new Map();
  writes = [];
  requests = [];
  vi.stubGlobal('location', { port: '7326', protocol: 'https:' });
  vi.stubGlobal('document', {
    get cookie() {
      return [...cookies].map(([key, value]) => `${key}=${value}`).join('; ');
    },

    set cookie(value: string) {
      writes.push(value);
      const entry = value.split(';')[0]!;
      const split = entry.indexOf('=');
      const key = entry.slice(0, split);
      if (value.includes('Max-Age=0')) cookies.delete(key);
      else cookies.set(key, entry.slice(split + 1));
    },
  });
  respond = async (config) => {
    if (config.url === '/api/auth/login')
      return response(config, { center_id: 'center-id', user, token: 'token-a', llm_key: 'never-retain-key' });
    return response(config, { center_id: 'center-id', user });
  };
  client.setConfig({
    adapter: async (config) => {
      requests.push(config);
      return respond(config);
    },
  });
  app = bootstrap();
});
afterEach(() => {
  app.dispose();
  client.setConfig({ adapter: undefined });
  vi.unstubAllGlobals();
});

describe('刷新恢复后台登录', () => {
  it('无 Cookie 不查询身份；登录仅保存 Token，会话 Cookie 限定路径及安全属性', async () => {
    expect(requests).toHaveLength(0);
    await app.store.login('admin', 'password');
    expect(readTokenCookie()).toBe('token-a');
    expect(writes).toEqual(['ez_admin_token_7326=token-a; Path=/admin/; SameSite=Strict; Secure']);
    expect(writes.join()).not.toMatch(/password|never-retain-key|display_name|Expires|Max-Age/);
  });
  it('应用重建保留 Cookie，首次渲染前进入恢复态，再用 Bearer 取得身份', async () => {
    await app.store.login('admin', 'password');
    app.dispose();
    expect(readTokenCookie()).toBe('token-a');
    expect(app.store.user).toBeUndefined();
    app = bootstrap();
    expect(app.store.restoreState).toBe('pending');
    expect(app.store.user).toBeUndefined();
    await vi.waitFor(() => expect(app.store.user?.id).toBe(1));
    expect(app.store.restoreState).toBe('none');
    expect(requests.at(-1)?.url).toBe('/api/auth/me');
    expect(requests.at(-1)?.headers.get('Authorization')).toBe('Bearer token-a');
    expect(requests.filter((request) => request.url === '/api/auth/login')).toHaveLength(1);
  });
  it.each(['TOKEN_INVALID', 'ADMIN_REQUIRED'])('恢复遇到 %s 清除当前 Cookie 和身份', async (code) => {
    writeTokenCookie('token-a');
    respond = async (config) => {
      throw new AxiosError('unsafe raw message', 'ERR_BAD_REQUEST', config, undefined, {
        ...response(config, { error: { code } }),
        status: 401,
      });
    };
    await app.store.restoreIdentity();
    expect(app.store.token).toBe('');
    expect(app.store.user).toBeUndefined();
    expect(app.store.restoreState).toBe('none');
    expect(readTokenCookie()).toBe('');
    expect(app.store.notice).not.toContain('unsafe');
  });
  it('恢复断网不清凭据，重试成功后进入后台', async () => {
    writeTokenCookie('token-a');
    const normal = respond;
    respond = async () => {
      throw new AxiosError('network');
    };
    await app.store.restoreIdentity();
    expect(app.store.restoreState).toBe('failed');
    expect(app.store.user).toBeUndefined();
    expect(app.store.token).toBe('token-a');
    expect(readTokenCookie()).toBe('token-a');
    respond = normal;
    await app.store.verifyIdentity();
    expect(app.store.restoreState).toBe('none');
    expect(app.store.user?.id).toBe(1);
  });
  it.each([
    { ...user, enabled: false },
    { ...user, role: 'user', is_super_admin: false },
  ])('恢复不接受停用或无管理权限的身份 %#', async (identity) => {
    writeTokenCookie('token-a');
    respond = async (config) => response(config, { center_id: 'center-id', user: identity });
    await app.store.restoreIdentity();
    expect(app.store.user).toBeUndefined();
    expect(readTokenCookie()).toBe('');
  });
  it('退出失败保留 Cookie 供重试，成功后清除', async () => {
    await app.store.login('admin', 'password');
    const normal = respond;
    respond = async () => {
      throw new AxiosError('network');
    };
    await app.store.logout();
    expect(readTokenCookie()).toBe('token-a');
    expect(app.store.logoutState).toBe('failed');
    respond = normal;
    await app.store.logout();
    expect(readTokenCookie()).toBe('');
  });
  it('本人改密成功保留 Cookie', async () => {
    await app.store.login('admin', 'password');
    await app.store.changePassword('old-password', 'new-password');
    expect(readTokenCookie()).toBe('token-a');
  });
  it('恢复的迟到响应不能覆盖之后的登录身份', async () => {
    writeTokenCookie('token-a');
    let finish!: (response: AxiosResponse) => void;
    let previous!: InternalAxiosRequestConfig;
    respond = (config) => {
      if (config.url === '/api/auth/me') {
        previous = config;
        return new Promise((resolve) => {
          finish = resolve;
        });
      }
      return Promise.resolve(response(config, { center_id: 'center-id', user, token: 'token-b' }));
    };
    const restoring = app.store.restoreIdentity();
    await vi.waitFor(() => expect(finish).toBeTypeOf('function'));
    await app.store.login('admin', 'password');
    finish(response(previous, { center_id: 'center-id', user: { ...user, id: 2 } }));
    await restoring;
    expect(app.store.user?.id).toBe(1);
    expect(app.store.token).toBe('token-b');
    expect(readTokenCookie()).toBe('token-b');
    expect(app.store.restoreState).toBe('none');
  });
  it('旧标签页失效不删除新 Cookie；不同端口使用不同名称', () => {
    writeTokenCookie('new-token');
    clearTokenCookie('old-token');
    expect(readTokenCookie()).toBe('new-token');
    vi.stubGlobal('location', { port: '7324', protocol: 'http:' });
    expect(readTokenCookie()).toBe('');
    writeTokenCookie('other-token');
    expect(writes.at(-1)).not.toContain('Secure');
    expect(cookies.size).toBe(2);
  });
});
