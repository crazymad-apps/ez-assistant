import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { NetworkStreams } from '../../dist/host-devices/streams.js';
import { decodeFrame } from '../../dist/host-devices/stream-frame.js';
import { createHttpApp } from '../../dist/app.js';
import type { HostDeviceDetail, HostDeviceState } from '../../dist/contracts/host-devices.js';

const root = resolve(process.env.CENTER_F02_SCENARIO ?? '');
if (!process.env.CENTER_F02_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f02-m0-')))
  throw new Error('必须使用全新 M0 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string;
  postgres_port: number;
  center_port: number;
  source_port: number;
  target_port: number;
};
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://f02_m0@127.0.0.1:${scenario.postgres_port}/f02_m0`;
const pool = createPool({ url }),
  source = createDataSource({ url });
const origin = `http://127.0.0.1:${scenario.center_port}`;
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
let admin = '',
  center = '';
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
async function eventually<T>(read: () => Promise<T>, check: (value: T) => boolean, ms = 15000): Promise<T> {
  const until = Date.now() + ms;
  let value: T;
  do {
    value = await read();
    if (check(value)) return value;
    await delay(100);
  } while (Date.now() < until);
  throw new Error(`state did not converge: ${JSON.stringify(value!)}`);
}
class Host {
  child?: ChildProcess;
  bootstrap = '';
  constructor(
    readonly name: string,
    readonly port: number,
  ) {}
  get home() {
    return join(root, this.name);
  }
  async start() {
    await mkdir(this.home, { mode: 0o700 });
    await writeFile(
      join(this.home, 'host.toml'),
      `version = "0.27.0"\nmode = "enterprise"\n[enterprise]\ncenter_url = "${origin}"\ncenter_id = "${center}"\n[host_access]\nport = ${this.port}\nremote_enabled = false\nscheme = "http"\nserver_names = []\n`,
      { mode: 0o600 },
    );
    this.child = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', this.home], {
      stdio: 'ignore',
    });
    await eventually(
      async () => {
        if (this.child!.exitCode !== null) throw new Error('Host exited');
        try {
          return JSON.parse(await readFile(join(this.home, 'run/runtime.json'), 'utf8')) as {
            pid: number;
            access_token: string;
          };
        } catch {
          return { pid: 0, access_token: '' };
        }
      },
      (v) => {
        if (v.pid !== this.child!.pid) return false;
        this.bootstrap = v.access_token;
        return true;
      },
    );
  }
  async stop() {
    if (!this.child || this.child.exitCode !== null) return;
    const stopped = once(this.child, 'exit');
    this.child.kill('SIGINT');
    await Promise.race([
      stopped,
      delay(10000).then(() => {
        throw new Error('Host shutdown timeout');
      }),
    ]);
  }
  async request(path: string, body: unknown, token = this.bootstrap, extra: Record<string, string> = {}) {
    return fetch(`http://127.0.0.1:${this.port}${path}`, {
      method: 'POST',
      headers: {
        ...headers(token),
        'x-ez-client-version': '0.32.0',
        'x-ez-min-compatible-version': '0.32.0',
        'x-ez-host-bootstrap': this.bootstrap,
        'content-type': 'application/json',
        ...extra,
      },
      body: JSON.stringify(body),
    });
  }
  async login(username: string) {
    const response = await this.request('/auth/login', {
      method: 'enterprise',
      username,
      password: username === 'admin' ? '123456' : 'test1234',
      native: true,
    });
    expect(response.status).toBe(200);
    return ((await response.json()) as { token: string }).token;
  }
  async access(token: string, type: string, payload: object) {
    const response = await this.request('/host-network/access', { type, payload }, token);
    const value = (await response.json()) as {
      type: string;
      payload?: { access_id: string; active: boolean; items: { id: string }[] };
    };
    expect(response.status, JSON.stringify(value)).toBe(200);
    return value;
  }
}
const a = new Host('source', scenario.source_port),
  b = new Host('target', scenario.target_port);
async function device(id: string) {
  const response = await app!.inject({ url: `/api/admin/host-devices/${id}`, headers: headers(admin) });
  expect(response.statusCode).toBe(200);
  return response.json<HostDeviceDetail>();
}
async function manage(id: string, patch: Partial<HostDeviceState>) {
  const d = await device(id),
    expected = { status: d.status, name: d.name, enabled: d.enabled, users: d.users };
  const response = await app!.inject({
    method: 'PATCH',
    url: `/api/admin/host-devices/${id}`,
    headers: headers(admin),
    payload: { expected, desired: { ...expected, ...patch } },
  });
  expect(response.statusCode, response.body).toBe(200);
}
async function loginCenter() {
  const response = await app!.inject({
    method: 'POST',
    url: '/api/auth/login',
    payload: { username: 'admin', password: '123456' },
  });
  expect(response.statusCode).toBe(200);
  const login = response.json<{ token: string; center_id: string }>();
  admin = login.token;
  center = login.center_id;
}
afterAll(async () => {
  await a.stop();
  await b.stop();
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
});

function command(type: string, payload: object = {}) {
  return { request_id: `m0-${type}`, command: { scope: 'runtime', payload: { type, payload } } };
}
async function call(host: Host, token: string, type: string, payload: object = {}, target?: string) {
  const response = await host.request(
    '/commands',
    command(type, payload),
    token,
    target ? { 'x-ez-target-device': target } : {},
  );
  const value = (await response.json()) as {
    request_id: string;
    result: { payload: { payload: Record<string, any> } };
  };
  expect(response.status, JSON.stringify(value)).toBe(200);
  expect(value.request_id).toBe(`m0-${type}`);
  const result = value.result.payload.payload;
  // 主控会话由 Runtime 自动建立；归属断言只比较本测试创建的普通会话。
  if (type === 'list_sessions')
    result.sessions = result.sessions.filter((s: { role: string }) => s.role !== 'controller');
  return result;
}
it('原 Command 经中心进入目标本人 Runtime，查询和修改不落到调用端', async () => {
  expect(
    (
      await pool.query(
        "SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir",
      )
    ).rows[0],
  ).toEqual({
    host: '127.0.0.1',
    port: scenario.postgres_port,
    db: 'f02_m0',
    role: 'f02_m0',
    dir: join(root, 'pgdata'),
  });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await upgradeDatabase(pool);
  await source.initialize();
  app = await createHttpApp({ source, origin });
  await app.listen(scenario.center_port, '127.0.0.1');
  await loginCenter();
  const ids: number[] = [];
  for (const username of ['user-a', 'user-b']) {
    const response = await app.inject({
      method: 'POST',
      url: '/api/users',
      headers: headers(admin),
      payload: { username, display_name: username, password: 'test1234', role: 'user', enabled: true },
    });
    expect(response.statusCode).toBe(201);
    ids.push(response.json<{ id: number }>().id);
  }
  await a.start();
  await b.start();
  const targetAdmin = await b.login('admin');
  const applied = await b.request(
    '/host-network/registration',
    { type: 'apply', payload: { name: 'M0 target' } },
    targetAdmin,
  );
  expect(applied.status).toBe(200);
  const id = ((await applied.json()) as { registration_id: string }).registration_id;
  const grants = ids.map((user_id) => ({ user_id, desktop_view: false, desktop_control: false }));
  await manage(id, { status: 'registered', enabled: true, users: grants });
  await eventually(
    () => device(id),
    (d) => d.online,
  );
  const userA = await a.login('user-a'),
    userB = await a.login('user-b'),
    adminA = await a.login('admin');
  const caps = await fetch(`http://127.0.0.1:${a.port}/capabilities`, { headers: headers(userA) });
  expect(caps.headers.get('x-ez-network-proxy')).toBe('2');
  const registration = await a.request('/host-network/registration', { type: 'get_status' });
  expect(await registration.json()).toMatchObject({ registration_id: null });
  // 同键并发首次请求合并访问；调用端尚未登记也可以作为已授权用户发起访问。
  const queries = await Promise.all(Array.from({ length: 4 }, () => call(a, userA, 'list_workspaces', {}, id)));
  for (const query of queries) expect(query.workspaces).toEqual([]);
  expect(
    (
      await pool.query("SELECT COUNT(*) FROM management_audit WHERE action='device_user_login' AND actor_user_id=$1", [
        ids[0],
      ])
    ).rows[0].count,
  ).toBe('1');
  const workspace = (
    await call(
      a,
      userA,
      'register_workspace',
      { label: 'remote workspace', primary_directory: null, additional_directories: [] },
      id,
    )
  ).workspace;
  const created = (
    await call(a, userA, 'create_session', { title: 'remote original', workspace_id: workspace.workspace_id }, id)
  ).session;
  await call(a, userA, 'rename_session', { session_id: created.session_id, title: 'remote renamed' }, id);
  const directA = await b.login('user-a'),
    directB = await b.login('user-b');
  expect((await call(b, directA, 'get_session', { session_id: created.session_id })).session.title).toBe(
    'remote renamed',
  );
  expect(
    (await call(b, directA, 'list_workspaces')).workspaces.map((w: { workspace_id: string }) => w.workspace_id),
  ).toEqual([workspace.workspace_id]);
  expect((await call(a, userA, 'list_workspaces')).workspaces).toEqual([]);
  expect((await call(a, userA, 'list_sessions')).sessions).toEqual([]);
  expect((await call(a, userB, 'list_sessions', {}, id)).sessions).toEqual([]);
  expect((await call(b, directB, 'list_sessions')).sessions).toEqual([]);
  // 请求超过一帧，响应超过一帧，仍保留原业务错误和关联 ID。
  const large = await a.request(
    '/commands',
    command('register_workspace', { label: 'x'.repeat(90000), additional_directories: [] }),
    userA,
    { 'x-ez-target-device': id },
  );
  expect(large.status).toBe(400);
  expect(await large.json()).toHaveProperty('request_id', 'm0-register_workspace');
  const queryReply = await a.request(
    `/commands?target_device=${id}`,
    command('get_session', { session_id: created.session_id }),
    userA,
  );
  expect(queryReply.status).toBe(200);
  await queryReply.json();
  for (const type of ['shutdown_runtime', 'reload_config']) {
    expect((await a.request('/commands', command(type), userA, { 'x-ez-target-device': id })).status).toBe(403);
  }
  expect((await a.request('/commands', command('list_sessions'), adminA, { 'x-ez-target-device': id })).status).toBe(
    403,
  );
  expect(
    (await a.request('/commands', command('list_sessions'), a.bootstrap, { 'x-ez-target-device': id })).status,
  ).toBe(403);
  expect(
    (await a.request('/commands?target_device=invalid', command('list_sessions'), userA, { 'x-ez-target-device': id }))
      .status,
  ).toBe(400);
  expect(
    (
      await a.request('/commands', command('list_sessions'), userA, {
        'x-ez-target-device': id,
        'x-ez-client-version': '0.1.0',
      })
    ).status,
  ).toBe(409);
  expect((await a.request('/events', {}, userA, { 'x-ez-target-device': id })).status).toBe(403);
  // 正文已到齐但正常 FIN 丢失，不能被 HTTP Content-Length 掩盖为成功。
  const relay = NetworkStreams.prototype.binary;
  let cutFin = false;
  const missingFin = vi.spyOn(NetworkStreams.prototype, 'binary').mockImplementation(function (
    this: NetworkStreams,
    peer,
    bytes,
  ) {
    if (!cutFin && peer.id === id && decodeFrame(bytes).kind === 4) {
      cutFin = true;
      peer.stop();
      return;
    }
    return relay.call(this, peer, bytes);
  });
  try {
    const read = async () => {
      const response = await a.request('/commands', command('list_sessions'), userA, { 'x-ez-target-device': id });
      expect(response.status).toBe(200);
      return response.json();
    };
    await expect(read()).rejects.toThrow();
    expect(cutFin).toBe(true);
  } finally {
    missingFin.mockRestore();
  }
  await eventually(
    () => device(id),
    (d) => d.online,
  );
  // 写入已提交，但目标响应头尚未穿过中心时切断连接：只能报告未知，不能重放。
  const forwardBinary = NetworkStreams.prototype.binary;
  let dropped = false;
  const lostResponse = vi.spyOn(NetworkStreams.prototype, 'binary').mockImplementation(function (
    this: NetworkStreams,
    peer,
    bytes,
  ) {
    if (!dropped && decodeFrame(bytes).kind === 2) {
      dropped = true;
      peer.stop();
      return;
    }
    return forwardBinary.call(this, peer, bytes);
  });
  try {
    const response = await a.request('/commands', command('create_session', { title: 'committed once' }), userA, {
      'x-ez-target-device': id,
    });
    expect(response.status).toBe(502);
    expect(await response.json()).toMatchObject({ error: { outcome: 'unknown' } });
  } finally {
    lostResponse.mockRestore();
  }
  expect(
    (await call(b, directA, 'list_sessions')).sessions.filter((s: { title: string }) => s.title === 'committed once'),
  ).toHaveLength(1);
  await eventually(
    () => device(id),
    (d) => d.online,
  );
  // 调用端完成登记后继续复用同一代理规则。
  const appliedA = await a.request(
    '/host-network/registration',
    { type: 'apply', payload: { name: 'M0 source' } },
    adminA,
  );
  expect(appliedA.status).toBe(200);
  const sourceId = ((await appliedA.json()) as { registration_id: string }).registration_id;
  await manage(sourceId, { status: 'registered', enabled: true, users: [] });
  await eventually(
    () => device(sourceId),
    (d) => d.online,
  );
  expect((await call(a, userA, 'list_sessions', {}, id)).sessions).toHaveLength(2);
  // 撤销一名用户不影响另一用户、目标本机登录和心跳。
  await manage(id, { users: grants.filter((g) => g.user_id === ids[1]) });
  await eventually(
    async () => (await a.request('/commands', command('list_sessions'), userA, { 'x-ez-target-device': id })).status,
    (status) => status === 403,
  );
  expect((await call(a, userB, 'list_sessions', {}, id)).sessions).toEqual([]);
  expect((await call(b, directA, 'list_sessions')).sessions).toHaveLength(2);
  await b.stop();
  await eventually(
    () => device(id),
    (d) => !d.online,
  );
  const offline = await a.request('/commands', command('create_session', { title: 'must not replay' }), userB, {
    'x-ez-target-device': id,
  });
  expect(offline.status).not.toBe(200);
  expect(await offline.json()).toMatchObject({ error: { outcome: 'not_executed' } });
  expect((await call(a, userB, 'list_sessions')).sessions).toEqual([]);
  expect((await pool.query('SELECT COUNT(*) FROM users')).rows[0].count).toBe('3');
  expect((await pool.query('SELECT COUNT(*) FROM host_devices')).rows[0].count).toBe('2');
  expect((await pool.query('SELECT COUNT(*) FROM host_device_users')).rows[0].count).toBe('1');
}, 120000);
