import { randomUUID } from 'node:crypto';
import { afterAll, beforeAll, expect, it, vi } from 'vitest';
import type { EntityManager } from 'typeorm';
import { createHttpApp } from '../../dist/app.js';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { checkDatabase } from '../../dist/database/check.js';
import { IdentityService } from '../../dist/identity/service.js';
import { Passwords } from '../../dist/identity/password.js';
import type { LoginResponse } from '../../dist/contracts/identity.js';
import type { UserRecord } from '../../dist/contracts/users.js';

const url = process.env.CENTER_TEST_DATABASE_URL,
  adminUrl = process.env.CENTER_TEST_ADMIN_URL;
const restore = process.env.CENTER_TEST_RESTORE_DATABASE;
if (!url || !adminUrl || !restore || process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true')
  throw new Error('须先核验源/恢复库和独立备份');
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
  throw new Error('不是已核实的数字 ID 测试基线');
const base = createPool({ url }),
  admin = createPool({ url: adminUrl });
const pools: ReturnType<typeof createPool>[] = [],
  apps: Awaited<ReturnType<typeof createHttpApp>>[] = [];
const sources: ReturnType<typeof createDataSource>[] = [];
const counts = { center_identity: '1', users: '1', management_audit: '1', pgmigrations: '1' };

async function snapshot(pool: ReturnType<typeof createPool>) {
  const data: Record<string, unknown> = {};
  for (const table of Object.keys(counts))
    data[table] = (await pool.query(`SELECT * FROM public.${table} ORDER BY 1`)).rows;
  return data;
}

let baseline: Record<string, unknown>;
beforeAll(async () => {
  expect((await checkDatabase(base, true)).counts).toEqual(counts);
  baseline = await snapshot(base);
  const uri = new URL(url!);
  uri.pathname = '/' + restore;
  const restored = createPool({ url: uri.href });
  try {
    expect(await snapshot(restored)).toEqual(baseline);
  } finally {
    await restored.end();
  }
});
afterAll(async () => {
  await Promise.all(apps.map((app) => app.close()));
  await Promise.all(sources.filter((source) => source.isInitialized).map((source) => source.destroy()));
  expect(await snapshot(base)).toEqual(baseline);
  await Promise.all([base.end(), admin.end(), ...pools.map((pool) => pool.end())]);
});

async function fixture() {
  const name = 'ez_center_c01_test_m2_' + randomUUID().replaceAll('-', '');
  expect((await admin.query('SELECT datname FROM pg_database WHERE datname=$1', [name])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${name}" OWNER center_app TEMPLATE "${restore}"`);
  const uri = new URL(url!);
  uri.pathname = '/' + name;
  const pool = createPool({ url: uri.href });
  pools.push(pool);
  expect(await snapshot(pool)).toEqual(baseline);
  const source = await createDataSource({ url: uri.href }).initialize();
  sources.push(source);
  const app = await createHttpApp({ source, origin: 'https://center.example' });
  apps.push(app);
  return { pool, app, source };
}

type Fixture = Awaited<ReturnType<typeof fixture>>;
const password = 'safe-password-123';

function request(f: Fixture, token: string, method: 'GET' | 'POST' | 'PATCH' | 'DELETE', path: string, body?: object) {
  return f.app.inject({
    method,
    url: path,
    headers: { authorization: `Bearer ${token}` },
    ...(body === undefined ? {} : { payload: body }),
  });
}

async function login(f: Fixture, username = 'admin', secret = '123456'): Promise<LoginResponse> {
  const response = await f.app.inject({
    method: 'POST',
    url: '/api/auth/login',
    payload: { username, password: secret },
  });
  expect(response.statusCode).toBe(200);
  return response.json<LoginResponse>();
}

async function create(f: Fixture, token: string, username: string, role = 'user'): Promise<UserRecord> {
  const response = await request(f, token, 'POST', '/api/users', {
    username,
    display_name: username,
    role,
    enabled: true,
    password,
  });
  expect(response.statusCode).toBe(201);
  return response.json<UserRecord>();
}

const me = (f: Fixture, token: string) => request(f, token, 'GET', '/api/auth/me');

const patch = (f: Fixture, token: string, id: number, body: object) =>
  request(f, token, 'PATCH', '/api/users/' + id, body);

const reset = (f: Fixture, token: string, id: number, newPassword = 'reset-password-123') =>
  request(f, token, 'POST', `/api/users/${id}/reset-password`, { new_password: newPassword });

it('用户和审计由数据库生成递增整数，关联与详情接口全链路保持数字', async () => {
  const f = await fixture(),
    root = await login(f);
  const alice = await create(f, root.token, 'alice'),
    bob = await create(f, root.token, 'bob');
  expect([root.user.id, alice.id, bob.id]).toEqual([1, 2, 3]);
  expect(
    (
      await f.pool.query(`SELECT table_name,data_type,is_identity,identity_generation FROM information_schema.columns
    WHERE table_schema='public' AND table_name IN ('users','management_audit') AND column_name='id' ORDER BY table_name`)
    ).rows,
  ).toEqual(
    ['management_audit', 'users'].map((table_name) => ({
      table_name,
      data_type: 'integer',
      is_identity: 'YES',
      identity_generation: 'ALWAYS',
    })),
  );
  const rows = (await f.pool.query('SELECT id,actor_user_id,target_user_id FROM management_audit ORDER BY id')).rows;
  expect(rows).toEqual([
    { id: 1, actor_user_id: 1, target_user_id: 1 },
    { id: 2, actor_user_id: 1, target_user_id: 1 },
    { id: 3, actor_user_id: 1, target_user_id: 2 },
    { id: 4, actor_user_id: 1, target_user_id: 3 },
  ]);
  const detail = await request(f, root.token, 'GET', '/api/audit?id=4&actor_user_id=1');
  expect(detail.statusCode).toBe(200);
  expect(detail.json()).toMatchObject({
    total: 1,
    items: [{ id: 4, actor_user_id: 1, target_user_id: 3, actor_username: 'admin', target_username: 'bob' }],
  });
  expect((await request(f, root.token, 'GET', '/api/audit?id=2147483647')).json()).toMatchObject({
    total: 0,
    items: [],
  });
  expect(
    (
      await f.pool.query(`SELECT (SELECT count(*) FROM users) AS users,
    (SELECT count(*) FROM management_audit) AS management_audit,
    (SELECT count(*) FROM center_identity) AS center_identity, (SELECT count(*) FROM pgmigrations) AS pgmigrations`)
    ).rows,
  ).toEqual([{ users: '3', management_audit: '4', center_identity: '1', pgmigrations: '1' }]);
});

/** 仅在从已恢复备份复制的独立夹具中模拟人工赋权/故障；行数异常直接回滚并使测试失败。 */
async function sql(f: Fixture, statement: string, values: unknown[] = [], affected?: number) {
  const client = await f.pool.connect();
  try {
    await client.query('BEGIN');
    const result = await client.query(statement, values);
    if (affected !== undefined) expect(result.rowCount).toBe(affected);
    await client.query('COMMIT');
  } catch (error) {
    await client.query('ROLLBACK');
    throw error;
  } finally {
    client.release();
  }
}

it('真实创建/检索/分页/显示名编辑；无字段透传，无变化不重复写审计', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, ' Alice ');
  expect(user).toMatchObject({ username: 'alice', is_super_admin: false, role: 'user', enabled: true });
  expect(user.created_at).toMatch(/^\d{4}-/);
  expect(user).not.toHaveProperty('password_hash');
  const alice = await login(f, 'alice', password);
  const renamed = await patch(f, root.token, user.id, { display_name: '新名称' });
  expect(renamed.statusCode).toBe(200);
  expect((await me(f, alice.token)).json().user.display_name).toBe('新名称');
  const before = await snapshot(f.pool);
  expect((await patch(f, root.token, user.id, { display_name: '新名称' })).json()).toEqual(renamed.json());
  expect(await snapshot(f.pool)).toEqual(before);
  const page = (
    await request(f, root.token, 'GET', '/api/users?search=%E6%96%B0&role=user&enabled=true&limit=1')
  ).json();
  expect(page).toMatchObject({ total: 1, limit: 1, offset: 0 });
  expect(page.items[0].id).toBe(user.id);
  const empty = (await request(f, root.token, 'GET', '/api/users?offset=100')).json();
  expect(empty).toMatchObject({ items: [], total: 2 });
  expect((await request(f, root.token, 'GET', '/api/users?search=%25')).json().total).toBe(0);
  const row = (await f.pool.query('SELECT password_hash FROM users WHERE id=$1', [user.id])).rows[0];
  expect(await new Passwords().verify(password, row.password_hash)).toBe(true);
});

it('数据库赋予任意用户超级管理员标记立即生效；不绑定初始化用户名/ID或role文本', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'second');
  const second = await login(f, 'second', password);
  expect((await request(f, second.token, 'GET', '/api/users')).statusCode).toBe(403);
  await sql(f, 'UPDATE users SET is_super_admin=true WHERE id=$1', [user.id], 1);
  expect((await request(f, second.token, 'GET', '/api/users')).statusCode).toBe(200);
  expect((await me(f, second.token)).json().user).toMatchObject({ role: 'user', is_super_admin: true });
  // 默认账号移除标记/角色后不再有额外权限；这是测试人工数据库修改，不开放产品入口。
  await sql(f, "UPDATE users SET is_super_admin=false,role='user' WHERE id=$1", [root.user.id], 1);
  expect((await request(f, root.token, 'GET', '/api/users')).statusCode).toBe(403);
  expect((await patch(f, second.token, user.id, { enabled: false })).json().error.code).toBe('SUPER_ADMIN_PROTECTED');
  expect((await patch(f, second.token, user.id, { role: 'admin' })).statusCode).toBe(200);
  expect((await me(f, second.token)).statusCode).toBe(401);
  const promoted = await login(f, 'second', password);
  expect((await patch(f, promoted.token, user.id, { role: 'user' })).json().error.code).toBe('SUPER_ADMIN_PROTECTED');
  expect((await patch(f, promoted.token, user.id, { display_name: '第二超管' })).statusCode).toBe(200);
  for (const target of [root.user.id, user.id]) {
    expect((await patch(f, promoted.token, target, { is_super_admin: false })).statusCode).toBe(400);
    expect((await request(f, promoted.token, 'DELETE', '/api/users/' + target)).statusCode).toBe(404);
  }
  expect((await f.pool.query('SELECT count(*) FROM users WHERE is_super_admin')).rows[0].count).toBe('1');
});

it('普通用户/无凭据/LLM key 不能管理用户或读取审计，失败审计保留已知操作者', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice'),
    alice = await login(f, 'alice', password);
  for (const path of ['/api/users', '/api/audit']) {
    expect((await request(f, alice.token, 'GET', path)).statusCode).toBe(403);
    expect((await request(f, 'missing', 'GET', path)).statusCode).toBe(401);
    expect((await request(f, root.llm_key, 'GET', path)).statusCode).toBe(401);
  }
  expect((await reset(f, alice.token, root.user.id)).statusCode).toBe(403);
  expect((await patch(f, alice.token, user.id, { role: 'admin' })).statusCode).toBe(403);
  const denied = (
    await f.pool.query("SELECT actor_user_id FROM management_audit WHERE action='user_password_reset' AND NOT success")
  ).rows;
  expect(denied).toEqual([{ actor_user_id: user.id }]);
});

it('管理重置可覆盖自己、其他管理员与超级管理员，无旧密码；区别于本人改密', async () => {
  const f = await fixture(),
    root = await login(f);
  await create(f, root.token, 'manager', 'admin');
  const manager = await login(f, 'manager', password),
    otherRoot = await login(f);
  expect((await reset(f, manager.token, root.user.id)).statusCode).toBe(204);
  for (const auth of [root, otherRoot]) {
    expect((await me(f, auth.token)).statusCode).toBe(200);
    await expect(f.app.get(IdentityService).llmIdentity(auth.llm_key)).resolves.toMatchObject({ user: auth.user });
  }
  expect((await me(f, manager.token)).statusCode).toBe(200);
  const freshRoot = await login(f, 'admin', 'reset-password-123');
  expect((await reset(f, freshRoot.token, manager.user.id)).statusCode).toBe(204);
  const freshManager = await login(f, 'manager', 'reset-password-123');
  expect((await reset(f, freshManager.token, freshManager.user.id, 'manager-self-reset1')).statusCode).toBe(204);
  expect((await me(f, freshManager.token)).statusCode).toBe(200);
  const current = await login(f, 'manager', 'manager-self-reset1');
  expect(
    (
      await request(f, current.token, 'POST', '/api/auth/password', {
        old_password: 'wrong',
        new_password: 'personal-new-password1',
      })
    ).statusCode,
  ).toBe(401);
  expect(
    (
      await request(f, current.token, 'POST', '/api/auth/password', {
        old_password: 'manager-self-reset1',
        new_password: 'personal-new-password1',
      })
    ).statusCode,
  ).toBe(204);
  expect(
    (
      await f.pool.query(
        "SELECT action,count(*)::int AS count FROM management_audit WHERE success AND action IN ('user_password_reset','self_password_changed') GROUP BY action ORDER BY action",
      )
    ).rows,
  ).toEqual([
    { action: 'self_password_changed', count: 1 },
    { action: 'user_password_reset', count: 3 },
  ]);
});

it('停用/启用和角色变更撤销全部旧Token，不复活旧key；显示名不撤销', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice');
  const a = await login(f, 'alice', password),
    b = await login(f, 'alice', password);
  expect((await patch(f, root.token, user.id, { enabled: false })).statusCode).toBe(200);
  expect((await me(f, a.token)).statusCode).toBe(401);
  expect((await me(f, b.token)).statusCode).toBe(401);
  expect((await patch(f, root.token, user.id, { enabled: true })).statusCode).toBe(200);
  await expect(f.app.get(IdentityService).llmIdentity(a.llm_key)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  const c = await login(f, 'alice', password);
  expect((await patch(f, root.token, user.id, { role: 'admin' })).statusCode).toBe(200);
  expect((await me(f, c.token)).statusCode).toBe(401);
  const d = await login(f, 'alice', password);
  expect((await request(f, d.token, 'GET', '/api/users')).statusCode).toBe(200);
  expect((await patch(f, root.token, user.id, { role: 'user' })).statusCode).toBe(200);
  expect((await me(f, d.token)).statusCode).toBe(401);
});

it('唯一账号并发创建与管理员互相降权串行重验，最后管理员不可移除', async () => {
  const f = await fixture(),
    root = await login(f);
  const body = { username: 'beta', display_name: 'Beta', role: 'admin', enabled: true, password };
  const created = await Promise.all([
    request(f, root.token, 'POST', '/api/users', body),
    request(f, root.token, 'POST', '/api/users', body),
  ]);
  expect(created.map((r) => r.statusCode).sort()).toEqual([201, 409]);
  const beta = await login(f, 'beta', password);
  await sql(f, 'UPDATE users SET is_super_admin=false WHERE id=$1', [root.user.id], 1);
  const changed = await Promise.all([
    patch(f, root.token, beta.user.id, { role: 'user' }),
    patch(f, beta.token, root.user.id, { role: 'user' }),
  ]);
  expect(changed.filter((r) => r.statusCode === 200)).toHaveLength(1);
  expect(changed.filter((r) => r.statusCode === 401 || r.statusCode === 403)).toHaveLength(1);
  const remaining = (await f.pool.query("SELECT id FROM users WHERE enabled AND (role='admin' OR is_super_admin)"))
    .rows;
  expect(remaining).toHaveLength(1);
  const current = remaining[0].id === root.user.id ? root : beta;
  expect((await patch(f, current.token, current.user.id, { enabled: false })).json().error.code).toBe(
    'LAST_ADMIN_REQUIRED',
  );
  expect((await patch(f, current.token, current.user.id, { role: 'user' })).json().error.code).toBe(
    'LAST_ADMIN_REQUIRED',
  );
});

it('输入/路径/筛选注入拒绝；审计只读且支持分页、日期、动作、操作者与结果过滤', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice');
  for (const body of [{ is_super_admin: true }, { username: 'other' }, {}, { enabled: 'false' }])
    expect((await patch(f, root.token, user.id, body)).statusCode).toBe(400);
  expect((await request(f, root.token, 'PATCH', '/api/users/invalid', { enabled: false })).statusCode).toBe(400);
  expect((await patch(f, root.token, 2147483647, { enabled: false })).json().error.code).toBe('USER_NOT_FOUND');
  expect((await request(f, root.token, 'GET', '/api/users?limit=1&limit=2')).statusCode).toBe(400);
  expect((await request(f, root.token, 'GET', '/api/users?search=%27%20OR%201%3D1--')).json().total).toBe(0);
  const filtered = await request(
    f,
    root.token,
    'GET',
    `/api/audit?actor_user_id=${root.user.id}&action=user_created&success=true&limit=1&from=2000-01-01T00:00:00Z&to=2100-01-01T00:00:00Z`,
  );
  expect(filtered.statusCode).toBe(200);
  expect(filtered.json()).toMatchObject({ total: 1, limit: 1, offset: 0 });
  expect(filtered.json().items[0]).toMatchObject({
    target_user_id: user.id,
    details: { after: { display_name: 'alice', role: 'user', enabled: true } },
  });
  expect(filtered.body).not.toMatch(/password|ct_|cl_/);
  const rejected = (await request(f, root.token, 'GET', '/api/audit?action=user_updated&success=false')).json();
  expect(rejected.total).toBe(6);
  expect((await request(f, root.token, 'GET', '/api/audit?action=user_created&offset=999')).json()).toMatchObject({
    items: [],
    total: 1,
  });
  expect((await request(f, root.token, 'POST', '/api/audit', {})).statusCode).toBe(404);
  await sql(
    f,
    "UPDATE management_audit SET details=$1 WHERE action='user_created'",
    [{ after: { display_name: 'alice', token: 'not-for-response' }, password_hash: 'not-for-response' }],
    1,
  );
  expect((await request(f, root.token, 'GET', '/api/audit?action=user_created')).body).not.toContain(
    'not-for-response',
  );
});

it('审计故障时创建/编辑/重置均回滚，现有Token不撤销', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice'),
    alice = await login(f, 'alice', password);
  const before = await snapshot(f.pool);
  await sql(
    f,
    `CREATE FUNCTION public.reject_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'intentional audit failure'; END $$;
    CREATE TRIGGER reject_audit BEFORE INSERT ON management_audit FOR EACH ROW EXECUTE FUNCTION public.reject_audit();`,
  );
  expect(
    (
      await request(f, root.token, 'POST', '/api/users', {
        username: 'ghost',
        display_name: 'Ghost',
        role: 'user',
        enabled: true,
        password,
      })
    ).statusCode,
  ).toBe(503);
  expect((await patch(f, root.token, user.id, { enabled: false })).statusCode).toBe(503);
  expect((await reset(f, root.token, user.id)).statusCode).toBe(503);
  expect(await snapshot(f.pool)).toEqual(before);
  expect((await me(f, alice.token)).statusCode).toBe(200);
});

it('受控交错：重置密码计算时操作者被降权，提交前拒绝且目标密码不变', async () => {
  const f = await fixture(),
    root = await login(f),
    managerUser = await create(f, root.token, 'manager', 'admin');
  const user = await create(f, root.token, 'alice'),
    manager = await login(f, 'manager', password);
  const before = (await f.pool.query('SELECT password_hash FROM users WHERE id=$1', [user.id])).rows;
  let reached!: () => void, resume!: () => void;
  const ready = new Promise<void>((resolve) => {
      reached = resolve;
    }),
    gate = new Promise<void>((resolve) => {
      resume = resolve;
    });
  const original = Passwords.prototype.hash;
  const spy = vi.spyOn(Passwords.prototype, 'hash').mockImplementation(async function (this: Passwords, value) {
    const hash = await original.call(this, value);
    reached();
    await gate;
    return hash;
  });
  const pending = reset(f, manager.token, user.id);
  try {
    await ready;
    expect((await patch(f, root.token, managerUser.id, { role: 'user' })).statusCode).toBe(200);
  } finally {
    resume();
    spy.mockRestore();
  }
  expect((await pending).statusCode).toBe(401);
  expect((await f.pool.query('SELECT password_hash FROM users WHERE id=$1', [user.id])).rows).toEqual(before);
});

it('受控交错：旧密码验证完成后管理重置，登录不签发过时Token', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice');
  let reached!: () => void, resume!: () => void;
  const ready = new Promise<void>((resolve) => {
      reached = resolve;
    }),
    gate = new Promise<void>((resolve) => {
      resume = resolve;
    });
  const original = Passwords.prototype.verify;
  const spy = vi.spyOn(Passwords.prototype, 'verify').mockImplementation(async function (
    this: Passwords,
    value,
    encoded,
  ) {
    const result = await original.call(this, value, encoded);
    reached();
    await gate;
    return result;
  });
  const pending = f.app.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'alice', password } });
  try {
    await ready;
    expect((await reset(f, root.token, user.id)).statusCode).toBe(204);
  } finally {
    resume();
    spy.mockRestore();
  }
  expect((await pending).statusCode).toBe(409);
  expect((await login(f, 'alice', 'reset-password-123')).user.id).toBe(user.id);
});

it('真实提交成功但客户端丢失COMMIT结果时不重试、清空Token，数据库成功事实可核验', async () => {
  const f = await fixture(),
    root = await login(f),
    user = await create(f, root.token, 'alice');
  const identity = f.app.get(IdentityService),
    write = identity.write.bind(identity);
  const spy = vi.spyOn(identity, 'write').mockImplementation(function <T>(
    work: (client: EntityManager) => Promise<T>,
    committed?: (result: T) => void,
  ) {
    return write(async (client) => {
      const result = await work(client),
        runner = client.queryRunner!;
      const query = runner.query.bind(runner);
      // 只包装本次写事务连接；底层 COMMIT 真实完成后模拟客户端传输异常。
      runner.query = (async (statement: string, values?: unknown[], structured?: boolean) => {
        const response = structured ? await query(statement, values, true) : await query(statement, values);
        if (statement === 'COMMIT') throw new Error('intentional commit response loss');
        return response;
      }) as typeof runner.query;
      return result;
    }, committed);
  });
  let response;
  try {
    response = await patch(f, root.token, user.id, { display_name: 'committed' });
  } finally {
    spy.mockRestore();
  }
  expect(response.statusCode).toBe(503);
  expect(response.body).not.toContain('intentional');
  expect((await me(f, root.token)).statusCode).toBe(401);
  expect((await f.pool.query('SELECT display_name FROM users WHERE id=$1', [user.id])).rows).toEqual([
    { display_name: 'committed' },
  ]);
  expect(
    (await f.pool.query("SELECT count(*) FROM management_audit WHERE action='user_updated' AND success")).rows[0].count,
  ).toBe('1');
});

it('ORM 初始化不改已有表/数据，不创建第二份升级账本；默认实体查询不读取密码摘要', async () => {
  const f = await fixture();
  expect(await snapshot(f.pool)).toEqual(baseline);
  expect(
    (await f.pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")).rows.map(
      (row) => row.tablename,
    ),
  ).toEqual(['center_identity', 'management_audit', 'pgmigrations', 'users']);
  const repository = f.source.getRepository('UserEntity');
  const user = await repository.findOneByOrFail({ username: 'admin' });
  expect(user.password_hash).toBeUndefined();
  expect(repository.createQueryBuilder('u').getSql()).not.toContain('password_hash');
  expect((await login(f)).user.is_super_admin).toBe(true);
  await f.app.close();
  await f.source.destroy();
  expect(f.source.isInitialized).toBe(false);
  expect(
    (
      await f.pool.query(
        "SELECT count(*)::int AS count FROM pg_stat_activity WHERE datname=current_database() AND application_name='ez-enterprise-center' AND pid<>pg_backend_pid()",
      )
    ).rows[0].count,
  ).toBe(0);
});
