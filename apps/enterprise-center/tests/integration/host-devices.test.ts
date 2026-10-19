import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomBytes, randomUUID } from 'node:crypto';
import { runner } from 'node-pg-migrate';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { HostDevicesService } from '../../dist/host-devices/service.js';
import { IdentityService } from '../../dist/identity/service.js';
import type { HostDeviceDetail, HostDeviceState } from '../../dist/contracts/host-devices.js';
import { exerciseHost } from './host-devices-host.js';

// 不接受开发连接串；必须是全新、无用户数据且实际目录匹配的隔离 PG 场景。
const root = resolve(process.env.CENTER_F01_SCENARIO ?? '');
if (!process.env.CENTER_F01_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f01-m0-')))
  throw new Error('须指定全新 F01 M0 场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string;
  postgres_host: string;
  postgres_port: number;
  database: string;
  role: string;
  center_port: number;
  host_port: number;
};
if (
  scenario.root !== root ||
  scenario.postgres_host !== '127.0.0.1' ||
  scenario.database !== 'f01_m0' ||
  scenario.role !== 'f01_m0'
)
  throw new Error('隔离目标不符');
const url = `postgres://f01_m0@127.0.0.1:${scenario.postgres_port}/f01_m0`;
const pool = createPool({ url }),
  source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
const origin = `http://127.0.0.1:${scenario.center_port}`;
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
const editable = (d: HostDeviceDetail): HostDeviceState => ({
  name: d.name,
  status: d.status,
  enabled: d.enabled,
  users: d.users,
});
async function login(username = 'admin') {
  const response = await app!.inject({
    method: 'POST',
    url: '/api/auth/login',
    payload: { username, password: username === 'admin' ? '123456' : 'test1234' },
  });
  expect(response.statusCode).toBe(200);
  return response.json<{ token: string; center_id: string; user: { id: number } }>();
}
afterAll(async () => {
  vi.restoreAllMocks();
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
});

it('隔离升级、真实中心事务和正式 Host 登记恢复', async () => {
  const actual = (
    await pool.query(
      "SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir",
    )
  ).rows[0];
  expect(actual).toEqual({
    host: scenario.postgres_host,
    port: scenario.postgres_port,
    db: scenario.database,
    role: scenario.role,
    dir: join(root, 'pgdata'),
  });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  const connection = await pool.connect();
  try {
    await runner({
      dbClient: connection,
      dir: upgradeFiles().directory,
      direction: 'up',
      count: 11,
      migrationsTable: 'pgmigrations',
      schema: 'public',
      migrationsSchema: 'public',
      singleTransaction: true,
      logger: { info() {}, warn() {}, error() {} },
    });
  } finally {
    connection.release(true);
  }
  const before = (await pool.query('SELECT * FROM public.users')).rows;
  const centerBefore = (await pool.query('SELECT * FROM public.center_identity')).rows;
  expect((await upgradeDatabase(pool)).applied).toEqual(['012-host-devices']);
  expect((await pool.query('SELECT * FROM public.users')).rows).toEqual(before);
  expect((await pool.query('SELECT * FROM public.center_identity')).rows).toEqual(centerBefore);
  expect((await upgradeDatabase(pool)).applied).toEqual([]);
  await source.initialize();
  app = await createHttpApp({ source, origin });
  await app.listen(scenario.center_port, '127.0.0.1');
  let admin = await login();
  for (const username of ['device-a', 'device-b']) {
    expect(
      (
        await app.inject({
          method: 'POST',
          url: '/api/users',
          headers: headers(admin.token),
          payload: {
            username,
            display_name: username,
            role: 'user',
            enabled: true,
            password: 'test1234',
          },
        })
      ).statusCode,
    ).toBe(201);
  }
  const a = await login('device-a'),
    b = await login('device-b');
  const input = {
    center_id: admin.center_id,
    id: randomUUID(),
    installation_id: randomUUID(),
    secret: randomBytes(32).toString('hex'),
    name: '办公电脑',
    platform: 'macos',
    host_version: '0.31.0',
  };
  const apply = (body = input, token = a.token) =>
    app!.inject({ method: 'POST', url: '/api/host-devices', headers: headers(token), payload: body });
  const own = (id = input.id, token = input.secret) =>
    app!.inject({ url: `/api/host-devices/${id}/registration`, headers: headers(token) });
  const get = (id = input.id) => app!.inject({ url: `/api/admin/host-devices/${id}`, headers: headers(admin.token) });
  const change = (expected: HostDeviceState, desired: HostDeviceState, id = input.id, token = admin.token) =>
    app!.inject({
      method: 'PATCH',
      url: `/api/admin/host-devices/${id}`,
      headers: headers(token),
      payload: { expected, desired },
    });
  const end = (action: 'withdraw' | 'exit', id = input.id) =>
    app!.inject({
      method: 'POST',
      url: `/api/host-devices/${id}/registration`,
      headers: headers(input.secret),
      payload: { action },
    });
  expect((await own()).json().device).toBeNull();
  const duplicates = await Promise.all(Array.from({ length: 5 }, () => apply()));
  expect(duplicates.map((r) => r.statusCode)).toEqual(Array(5).fill(200));
  expect((await pool.query('SELECT COUNT(*) FROM public.host_devices')).rows[0].count).toBe('1');
  expect(
    (await pool.query("SELECT COUNT(*) FROM public.management_audit WHERE action='device_applied' AND success")).rows[0]
      .count,
  ).toBe('1');
  const stored = (await pool.query('SELECT credential_hash FROM public.host_devices')).rows[0].credential_hash;
  expect(stored).not.toBe(input.secret);
  expect((await get()).body).not.toContain(stored);
  expect((await own()).body).not.toContain(input.secret);
  expect((await apply({ ...input, secret: randomBytes(32).toString('hex') })).statusCode).toBe(409);
  expect((await apply({ ...input, id: randomUUID() })).statusCode).toBe(409);
  expect((await apply({ ...input, center_id: randomUUID() })).statusCode).toBe(409);
  expect((await own(input.id, a.token)).statusCode).toBe(401);
  expect((await own(input.id, randomBytes(32).toString('hex'))).statusCode).toBe(401);
  expect((await app.inject({ url: '/api/auth/me', headers: headers(input.secret) })).statusCode).toBe(401);
  let current = editable((await get()).json());
  const approved: HostDeviceState = {
    ...current,
    status: 'registered',
    users: [{ user_id: a.user.id, desktop_view: true, desktop_control: false }],
  };
  expect((await change(current, approved, input.id, b.token)).statusCode).toBe(403);
  expect(
    (
      await change(current, {
        ...approved,
        users: [{ user_id: a.user.id, desktop_view: false, desktop_control: true }],
      })
    ).statusCode,
  ).toBe(400);
  expect(
    (
      await change(current, {
        ...approved,
        users: [{ user_id: 2147483647, desktop_view: false, desktop_control: false }],
      })
    ).statusCode,
  ).toBe(409);
  expect((await get()).json().status).toBe('pending');
  const rollback = vi.spyOn(app.get(IdentityService), 'audit').mockRejectedValueOnce(new Error('isolated rollback'));
  expect((await change(current, approved)).statusCode).toBe(503);
  rollback.mockRestore();
  expect((await get()).json().status).toBe('pending');
  expect((await pool.query('SELECT COUNT(*) FROM public.host_device_users')).rows[0].count).toBe('0');
  expect((await change(current, approved)).json()).toMatchObject({
    status: 'registered',
    online: false,
    users: approved.users,
  });
  expect((await end('withdraw')).statusCode).toBe(409);
  const changed = { ...approved, name: '新名称', enabled: false, users: [] };
  expect((await change(approved, changed)).statusCode).toBe(200);
  expect((await change(current, approved)).json()).toMatchObject(changed);
  expect((await change(approved, { ...approved, name: '过期修改' })).statusCode).toBe(409);
  expect((await apply()).json().device.name).toBe('新名称');
  expect(
    (await app.inject({ url: '/api/admin/host-devices?status=unregistered', headers: headers(admin.token) })).json().total,
  ).toBe(0);
  expect(
    (
      await app.inject({ url: '/api/admin/host-devices?status=registered&limit=1', headers: headers(admin.token) })
    ).json().total,
  ).toBe(1);
  expect((await app.inject({ url: '/api/admin/host-devices', headers: headers(admin.token) })).statusCode).toBe(400);
  expect(
    (await app.inject({ url: '/api/admin/host-devices?status=registered', headers: headers(b.token) })).statusCode,
  ).toBe(403);
  current = editable((await get()).json());
  expect(
    (await change(current, { ...current, users: [{ user_id: a.user.id, desktop_view: true, desktop_control: true }] }))
      .statusCode,
  ).toBe(200);
  expect(
    (await app.inject({ method: 'DELETE', url: `/api/users/${a.user.id}`, headers: headers(admin.token) })).statusCode,
  ).toBe(204);
  expect((await get()).json()).toMatchObject({ applicant_user_id: a.user.id, users: [] });
  expect((await end('exit')).json().device.status).toBe('revoked');
  expect((await end('exit')).json().device.status).toBe('revoked');
  expect((await apply(input, b.token)).json().device.status).toBe('revoked');
  const replacement = { ...input, id: randomUUID() };
  expect((await apply(replacement, b.token)).json().device.status).toBe('pending');
  const pending = editable((await get(replacement.id)).json());
  const race = await Promise.all([
    change(pending, { ...pending, status: 'registered' }, replacement.id),
    end('withdraw', replacement.id),
  ]);
  const finalStatus = (await get(replacement.id)).json().status;
  expect(['registered', 'withdrawn']).toContain(finalStatus);
  expect(race.filter((r) => r.statusCode === 200)).toHaveLength(1);
  expect(race.filter((r) => r.statusCode === 409)).toHaveLength(1);
  await app.close();
  app = await createHttpApp({ source, origin });
  await app.listen(scenario.center_port, '127.0.0.1');
  expect((await own()).json().device).toMatchObject({ status: 'revoked', online: false });
  admin = await login();
  // 正式 Host 子进程使用另一个全新 Runtime Home；中心重启不丢失登记事实。
  const devices = app.get(HostDevicesService);
  const originalApply = devices.apply.bind(devices);
  vi.spyOn(devices, 'apply').mockImplementationOnce(async (...args) => {
    await originalApply(...args);
    throw new Error('isolated response lost after commit');
  });
  await exerciseHost({
    root,
    origin,
    hostPort: scenario.host_port,
    centerId: admin.center_id,
    adminToken: admin.token,
    restartCenter: async () => {
      await app!.close();
      app = await createHttpApp({ source, origin });
      await app.listen(scenario.center_port, '127.0.0.1');
      admin = await login();
      return admin.token;
    },
    stopCenter: async () => {
      await app!.close();
    },
  });
  for (const table of ['host_devices', 'host_device_users', 'users', 'management_audit', 'pgmigrations']) {
    const count = (await pool.query(`SELECT COUNT(*) FROM public.${table}`)).rows[0].count;
    console.log(`F01 isolated ${table}: ${count}`);
  }
}, 120000);
