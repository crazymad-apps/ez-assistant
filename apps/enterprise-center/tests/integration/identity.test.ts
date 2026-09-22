import { randomUUID } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { afterAll, beforeAll, expect, it, vi } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { checkDatabase } from '../../dist/database/check.js';
import { IdentityService } from '../../dist/identity/service.js';
import { Passwords } from '../../dist/identity/password.js';
import type { LoginResponse } from '../../dist/contracts/identity.js';

const url = process.env.CENTER_TEST_DATABASE_URL;
const adminUrl = process.env.CENTER_TEST_ADMIN_URL;
const restore = process.env.CENTER_TEST_RESTORE_DATABASE;
if (!url || !adminUrl || !restore || process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true')
  throw new Error('须显式提供已核验隔离库及恢复备份');
const target = new URL(url),
  adminTarget = new URL(adminUrl);
if (
  target.hostname !== '127.0.0.1' ||
  target.port !== '55432' ||
  target.username !== 'center_app' ||
  !/^\/ez_center_c01_test_m1_[a-z0-9_]+$/.test(target.pathname) ||
  adminTarget.host !== target.host ||
  adminTarget.username !== 'postgres' ||
  adminTarget.pathname !== '/postgres' ||
  !/^ez_center_c01_restore_m1_[a-z0-9_]+$/.test(restore)
)
  throw new Error('不是已确认的 M1 隔离目标');
const origin = 'https://center.example';
const base = createPool({ url }),
  admin = createPool({ url: adminUrl });
const pools: ReturnType<typeof createPool>[] = [];
const sources: ReturnType<typeof createDataSource>[] = [];
const apps: Awaited<ReturnType<typeof createHttpApp>>[] = [];
const baselineCounts = { center_identity: '1', users: '1', management_audit: '1', pgmigrations: '1' };

async function snapshot(pool: ReturnType<typeof createPool>) {
  const result: Record<string, unknown> = {};
  for (const table of Object.keys(baselineCounts))
    result[table] = (await pool.query(`SELECT * FROM public.${table} ORDER BY 1`)).rows;
  return result;
}

beforeAll(async () => {
  expect((await checkDatabase(base, true)).counts).toEqual(baselineCounts);
  const uri = new URL(url!);
  uri.pathname = '/' + restore;
  const restored = createPool({ url: uri.href });
  try {
    expect(await snapshot(restored)).toEqual(await snapshot(base));
  } finally {
    await restored.end();
  }
});
afterAll(async () => {
  await Promise.all(apps.map((app) => app.close()));
  await Promise.all(sources.filter((source) => source.isInitialized).map((source) => source.destroy()));
  expect((await checkDatabase(base, true)).counts).toEqual(baselineCounts);
  await Promise.all([base.end(), admin.end(), ...pools.map((pool) => pool.end())]);
});

async function fixture() {
  const name = 'ez_center_c01_test_m1_' + randomUUID().replaceAll('-', '');
  expect((await admin.query('SELECT datname FROM pg_database WHERE datname=$1', [name])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${name}" OWNER center_app TEMPLATE "${restore}"`);
  const uri = new URL(url!);
  uri.pathname = '/' + name;
  const pool = createPool({ url: uri.href });
  pools.push(pool);
  expect(await snapshot(pool)).toEqual(await snapshot(base));
  const source = await createDataSource({ url: uri.href }).initialize();
  sources.push(source);
  const app = await createHttpApp({ source, origin });
  apps.push(app);
  return { pool, app, uri };
}

type Fixture = Awaited<ReturnType<typeof fixture>>;

async function login(f: Fixture, username = 'admin', password = '123456') {
  return f.app.inject({ method: 'POST', url: '/api/auth/login', payload: { username, password } });
}

async function host(f: Fixture, username = 'admin'): Promise<LoginResponse> {
  const response = await login(f, username);
  expect(response.statusCode).toBe(200);
  return response.json<LoginResponse>();
}

async function me(f: Fixture, token: string) {
  return f.app.inject({ method: 'GET', url: '/api/auth/me', headers: { authorization: `Bearer ${token}` } });
}

async function logout(f: Fixture, token: string) {
  return f.app.inject({
    method: 'POST',
    url: '/api/auth/logout',
    headers: { authorization: `Bearer ${token}` },
    payload: {},
  });
}

async function password(f: Fixture, token: string, old_password = '123456', new_password = 'new-password-123') {
  return f.app.inject({
    method: 'POST',
    url: '/api/auth/password',
    headers: { authorization: `Bearer ${token}` },
    payload: { old_password, new_password },
  });
}

async function ordinary(f: Fixture, enabled = true) {
  await f.pool.query(
    `INSERT INTO users(username,display_name,role,is_super_admin,enabled,password_hash)
    SELECT 'alice','Alice','user',false,$1,password_hash FROM users WHERE username='admin'`,
    [enabled],
  );
  expect((await f.pool.query('SELECT count(*) FROM users')).rows[0].count).toBe('2');
}

it('登录凭据仅在内存，数据库无会话表，me 不返回秘密，LLM key 不可用于身份 API', async () => {
  const f = await fixture(),
    result = await host(f);
  expect(result.token).toMatch(/^ct_[a-f0-9]{64}$/);
  expect(result.llm_key).toMatch(/^cl_[a-f0-9]{64}$/);
  expect((await f.pool.query("SELECT to_regclass('public.login_sessions') AS relation")).rows[0].relation).toBeNull();
  const identity = await me(f, result.token);
  expect(identity.statusCode).toBe(200);
  expect(identity.json()).toEqual({ center_id: result.center_id, user: result.user });
  expect(identity.headers['cache-control']).toBe('no-store');
  expect((await me(f, result.llm_key)).statusCode).toBe(401);
  const stored = JSON.stringify(await snapshot(f.pool));
  expect(stored).not.toContain(result.token);
  expect(stored).not.toContain(result.llm_key);
  const contract = JSON.parse(readFileSync('tests/fixtures/identity/host-login.json', 'utf8'));
  expect({
    ...result,
    center_id: '00000000-0000-4000-8000-000000000001',
    user: { ...result.user, id: 1 },
    token: 'ct_' + '0'.repeat(64),
    llm_key: 'cl_' + '0'.repeat(64),
  }).toEqual(contract);
});

it('普通用户与管理员统一 Bearer 登录；Cookie 不参与鉴权且后端不设置 Cookie', async () => {
  const f = await fixture();
  await ordinary(f);
  for (const username of ['admin', 'alice']) {
    const response = await login(f, username);
    expect(response.statusCode).toBe(200);
    expect(response.headers['set-cookie']).toBeUndefined();
    const result = response.json<LoginResponse>();
    expect((await me(f, result.token)).statusCode).toBe(200);
    expect(
      (await f.app.inject({ method: 'GET', url: '/api/auth/me', headers: { cookie: `token=${result.token}`, origin } }))
        .statusCode,
    ).toBe(401);
    expect(
      (
        await f.app.inject({
          method: 'GET',
          url: '/api/auth/me',
          headers: { cookie: 'theme=dark', authorization: `Bearer ${result.token}`, origin },
        })
      ).statusCode,
    ).toBe(200);
    const out = await logout(f, result.token);
    expect(out.statusCode).toBe(204);
    expect(out.headers['set-cookie']).toBeUndefined();
  }
});

it('未知账号、错误密码和停用账号统一失败，审计不保存输入内容', async () => {
  const f = await fixture();
  await ordinary(f, false);
  for (const [username, pwd] of [
    ['missing', 'secret-wrong'],
    ['admin', 'secret-wrong'],
    ['alice', '123456'],
  ]) {
    const response = await login(f, username, pwd);
    expect(response.statusCode).toBe(401);
    expect(response.json().error.code).toBe('AUTH_FAILED');
    expect({ error: { ...response.json().error, request_id: '00000000-0000-4000-8000-000000000004' } }).toEqual(
      JSON.parse(readFileSync('tests/fixtures/identity/auth-failed.json', 'utf8')),
    );
    expect(response.body).not.toContain(pwd!);
  }
  const audit = (
    await f.pool.query(
      "SELECT actor_user_id,target_user_id,reason_code,details FROM management_audit WHERE action='login'",
    )
  ).rows;
  expect(audit).toHaveLength(3);
  expect(
    audit.every(
      (row) => row.actor_user_id === null && row.target_user_id === null && row.reason_code === 'AUTH_FAILED',
    ),
  ).toBe(true);
  expect(JSON.stringify(audit)).not.toMatch(/missing|secret-wrong|123456/);
});

it('多次登录秘密不同；退出只撤销当前 Token 和关联 key，重复退出不重复审计', async () => {
  const f = await fixture(),
    a = await host(f),
    b = await host(f);
  expect(a.token).not.toBe(b.token);
  expect(a.llm_key).not.toBe(b.llm_key);
  const repeated = await Promise.all([logout(f, a.token), logout(f, a.token)]);
  expect(repeated.map((response) => response.statusCode)).toEqual([204, 204]);
  expect((await logout(f, 'ct_' + '0'.repeat(64))).statusCode).toBe(204);
  expect((await me(f, a.token)).statusCode).toBe(401);
  expect((await me(f, b.token)).statusCode).toBe(200);
  await expect(f.app.get(IdentityService).llmIdentity(a.llm_key)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  await expect(f.app.get(IdentityService).llmIdentity(b.llm_key)).resolves.toMatchObject({ user: b.user });
  expect(
    (await f.pool.query("SELECT count(*) FROM management_audit WHERE action='logout' AND success")).rows[0].count,
  ).toBe('1');
});

it('普通用户本人改密验证原密码，失败不撤销，成功保留全部 Token/key 且不影响其他人', async () => {
  const f = await fixture();
  await ordinary(f);
  const a = await host(f, 'alice'),
    b = await host(f, 'alice'),
    administrator = await host(f);
  const failed = await password(f, a.token, 'wrong');
  expect(failed.statusCode).toBe(401);
  expect((await me(f, a.token)).statusCode).toBe(200);
  expect((await password(f, a.token)).statusCode).toBe(204);
  for (const session of [a, b]) {
    expect((await me(f, session.token)).statusCode).toBe(200);
    await expect(f.app.get(IdentityService).llmIdentity(session.llm_key)).resolves.toMatchObject({
      user: session.user,
    });
  }
  expect((await me(f, administrator.token)).statusCode).toBe(200);
  expect((await login(f, 'alice')).statusCode).toBe(401);
  expect((await login(f, 'alice', 'new-password-123')).statusCode).toBe(200);
  const row = (await f.pool.query("SELECT password_hash FROM users WHERE username='alice'")).rows[0];
  expect(await new Passwords().verify('new-password-123', row.password_hash)).toBe(true);
});

it('管理员本人改密同样要求原密码', async () => {
  const f = await fixture(),
    result = await host(f);
  expect((await password(f, result.token, 'wrong')).statusCode).toBe(401);
  const changed = await password(f, result.token);
  expect(changed.statusCode).toBe(204);
  expect(changed.headers['set-cookie']).toBeUndefined();
  expect((await me(f, result.token)).statusCode).toBe(200);
});

it('未知字段、错误类型、非 JSON 与来源伪造不签发凭据', async () => {
  const f = await fixture();
  for (const payload of [
    { username: 'admin', password: '123456', is_super_admin: true },
    { username: 'admin', password: '123456', delivery: ['host'] },
  ]) {
    expect((await f.app.inject({ method: 'POST', url: '/api/auth/login', payload })).statusCode).toBe(400);
  }
  expect(
    (
      await f.app.inject({
        method: 'POST',
        url: '/api/auth/login',
        headers: { origin: 'https://evil.example' },
        payload: { username: 'admin', password: '123456' },
      })
    ).statusCode,
  ).toBe(403);
  expect(
    (
      await f.app.inject({
        method: 'POST',
        url: '/api/auth/login',
        headers: { 'content-type': 'text/plain' },
        payload: 'secret',
      })
    ).statusCode,
  ).toBe(400);
});

it('审计故障不签发 Token，回滚改密并保留已有 Token，不返回秘密或伪成功', async () => {
  const f = await fixture(),
    session = await host(f);
  const before = await snapshot(f.pool);
  // 已验证备份的独立夹具内制造固定审计错误，绝不作用于源/恢复库。
  await f.pool
    .query(`CREATE FUNCTION public.reject_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'intentional audit failure'; END $$;
    CREATE TRIGGER reject_audit BEFORE INSERT ON management_audit FOR EACH ROW EXECUTE FUNCTION public.reject_audit();`);
  const refused = await login(f);
  expect(refused.statusCode).toBe(503);
  expect(refused.body).not.toContain('intentional');
  expect((await password(f, session.token)).statusCode).toBe(503);
  expect((await logout(f, session.token)).statusCode).toBe(503);
  expect(await snapshot(f.pool)).toEqual(before);
  expect((await me(f, session.token)).statusCode).toBe(200);
});

it('受控交错：密码计算后账号停用，提交前重验拒绝签发', async () => {
  const f = await fixture();
  await ordinary(f);
  let computed!: () => void, resume!: () => void;
  const ready = new Promise<void>((resolve) => {
      computed = resolve;
    }),
    gate = new Promise<void>((resolve) => {
      resume = resolve;
    });
  const original = Passwords.prototype.verify;
  const spy = vi.spyOn(Passwords.prototype, 'verify').mockImplementation(async function (
    this: Passwords,
    password,
    encoded,
  ) {
    const result = await original.call(this, password, encoded);
    computed();
    await gate;
    return result;
  });
  const pending = login(f, 'alice');
  try {
    await ready;
    const c = await f.pool.connect();
    try {
      await c.query('BEGIN');
      await c.query("UPDATE users SET enabled=false WHERE username='alice'");
      await c.query('COMMIT');
    } finally {
      c.release();
    }
  } finally {
    resume();
    spy.mockRestore();
  }
  expect((await pending).statusCode).toBe(401);
});

it('受控交错：新密码计算期间退出，改密不能使用旧的认证结果提交', async () => {
  const f = await fixture(),
    session = await host(f);
  const before = (await f.pool.query('SELECT password_hash FROM users')).rows;
  let computed!: () => void, resume!: () => void;
  const ready = new Promise<void>((resolve) => {
      computed = resolve;
    }),
    gate = new Promise<void>((resolve) => {
      resume = resolve;
    });
  const original = Passwords.prototype.hash;
  const spy = vi.spyOn(Passwords.prototype, 'hash').mockImplementation(async function (this: Passwords, password) {
    const result = await original.call(this, password);
    computed();
    await gate;
    return result;
  });
  const pending = password(f, session.token);
  try {
    await ready;
    expect((await logout(f, session.token)).statusCode).toBe(204);
  } finally {
    resume();
    spy.mockRestore();
  }
  expect((await pending).statusCode).toBe(401);
  expect((await f.pool.query('SELECT password_hash FROM users')).rows).toEqual(before);
});

it('重建 HTTP 与数据库连接池后，旧 Token 和 key 全部失效', async () => {
  const f = await fixture(),
    a = await host(f),
    b = await host(f);
  expect((await logout(f, a.token)).statusCode).toBe(204);
  await f.app.close();
  const restartedPool = createPool({ url: f.uri.href });
  pools.push(restartedPool);
  const source = await createDataSource({ url: f.uri.href }).initialize();
  sources.push(source);
  const restarted = await createHttpApp({ source, origin });
  apps.push(restarted);
  const again = { ...f, app: restarted, pool: restartedPool };
  expect((await me(again, a.token)).statusCode).toBe(401);
  expect((await me(again, b.token)).statusCode).toBe(401);
  await expect(restarted.get(IdentityService).llmIdentity(b.llm_key)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  expect((await login(again)).statusCode).toBe(200);
});

it('入口限流忽略伪造转发地址，超过额度拒绝且不执行密码计算', async () => {
  const f = await fixture();
  for (let i = 0; i < 20; i++)
    expect(
      (
        await f.app.inject({
          method: 'POST',
          url: '/api/auth/login',
          headers: { 'x-forwarded-for': `192.0.2.${i}` },
          payload: {},
        })
      ).statusCode,
    ).toBe(400);
  const spy = vi.spyOn(Passwords.prototype, 'verify');
  try {
    expect((await login(f)).statusCode).toBe(429);
    expect(spy).not.toHaveBeenCalled();
  } finally {
    spy.mockRestore();
  }
});

it('实际产品进程重启后要求重新登录，不设置 Cookie', async () => {
  const f = await fixture();
  const server = createServer();
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('没有端口');
  const port = address.port;
  await new Promise<void>((resolve, reject) => server.close((error) => (error ? reject(error) : resolve())));
  const baseUrl = `http://127.0.0.1:${port}`;

  async function processService() {
    const child = spawn(process.execPath, ['dist/main.js', '--api-only'], {
      env: {
        ...process.env,
        CENTER_DATABASE_URL: f.uri.href,
        CENTER_DATABASE_AUTO_UPGRADE: 'false',
        CENTER_HOST: '127.0.0.1',
        CENTER_PORT: String(port),
        CENTER_PUBLIC_ORIGIN: baseUrl,
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const exited = new Promise<number | null>((resolve, reject) => {
      child.once('exit', resolve);
      child.once('error', reject);
    });
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        child.kill('SIGTERM');
        reject(new Error('产品启动超时'));
      }, 3000);
      child.once('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.stdout.on('data', (chunk) => {
        if (String(chunk).includes('center_listening')) {
          clearTimeout(timer);
          resolve();
        }
      });
    });
    return async () => {
      child.kill('SIGTERM');
      expect(await exited).toBe(0);
    };
  }

  const stopFirst = await processService();
  let token = '';
  try {
    const response = await fetch(`${baseUrl}/api/auth/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', origin: baseUrl },
      body: JSON.stringify({ username: 'admin', password: '123456' }),
    });
    expect(response.status).toBe(200);
    expect(response.headers.get('set-cookie')).toBeNull();
    token = ((await response.json()) as LoginResponse).token;
    expect((await fetch(`${baseUrl}/api/auth/me`, { headers: { authorization: `Bearer ${token}` } })).status).toBe(200);
  } finally {
    await stopFirst();
  }
  const stopSecond = await processService();
  try {
    expect((await fetch(`${baseUrl}/api/auth/me`, { headers: { authorization: `Bearer ${token}` } })).status).toBe(401);
  } finally {
    await stopSecond();
  }
});

it('并发本人改密只提交一份密码快照，同时保留两个已登录 Token/key', async () => {
  const f = await fixture(),
    a = await host(f),
    b = await host(f);
  let ready!: () => void,
    release!: () => void,
    computed = 0;
  const bothReady = new Promise<void>((resolve) => {
    ready = resolve;
  });
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  const original = Passwords.prototype.hash;
  const spy = vi.spyOn(Passwords.prototype, 'hash').mockImplementation(async function (this: Passwords, value) {
    const hash = await original.call(this, value);
    if (++computed === 2) ready();
    await gate;
    return hash;
  });
  const first = password(f, a.token, '123456', 'first-password1');
  const second = password(f, b.token, '123456', 'second-password1');
  try {
    await bothReady;
  } finally {
    release();
    spy.mockRestore();
  }
  const results = await Promise.all([first, second]);
  expect(results.map((result) => result.statusCode).sort()).toEqual([204, 409]);
  const expected = results[0]!.statusCode === 204 ? 'first-password1' : 'second-password1';
  const row = (await f.pool.query('SELECT password_hash FROM users')).rows[0];
  expect(await new Passwords().verify(expected, row.password_hash)).toBe(true);
  expect(await new Passwords().verify('123456', row.password_hash)).toBe(false);
  for (const session of [a, b]) {
    expect((await me(f, session.token)).statusCode).toBe(200);
    await expect(f.app.get(IdentityService).llmIdentity(session.llm_key)).resolves.toMatchObject({
      user: session.user,
    });
  }
  expect(
    (await f.pool.query("SELECT count(*) FROM management_audit WHERE action='self_password_changed' AND success"))
      .rows[0].count,
  ).toBe('1');
});
