import { readFile, mkdir, writeFile, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { ModelsService } from '../../dist/models/service.js';
import type { HostDeviceDetail, HostDeviceState } from '../../dist/contracts/host-devices.js';

const root = resolve(process.env.CENTER_F01_SCENARIO ?? '');
if (!process.env.CENTER_F01_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f01-m2-'))) throw new Error('必须使用全新 M2 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as { root: string; postgres_port: number; center_port: number; source_port: number; target_port: number };
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://f01_m2@127.0.0.1:${scenario.postgres_port}/f01_m2`;
const pool = createPool({ url }), source = createDataSource({ url });
const origin = `http://127.0.0.1:${scenario.center_port}`;
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
let admin = '', center = '';
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
async function eventually<T>(read: () => Promise<T>, check: (value: T) => boolean, ms = 15000): Promise<T> {
  const until = Date.now() + ms;
  let value: T;
  do { value = await read(); if (check(value)) return value; await delay(100); } while (Date.now() < until);
  throw new Error(`state did not converge: ${JSON.stringify(value!)}`);
}
class Host {
  child?: ChildProcess;
  bootstrap = '';
  constructor(readonly name: string, readonly port: number) {}
  get home() { return join(root, this.name); }
  async start() {
    await mkdir(this.home, { mode: 0o700 });
    await writeFile(join(this.home, 'host.toml'), `version = "0.27.0"\nmode = "enterprise"\n[enterprise]\ncenter_url = "${origin}"\ncenter_id = "${center}"\n[host_access]\nport = ${this.port}\nremote_enabled = false\nscheme = "http"\nserver_names = []\n`, { mode: 0o600 });
    this.child = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', this.home], { stdio: 'ignore' });
    await eventually(async () => {
      if (this.child!.exitCode !== null) throw new Error('Host exited');
      try { return JSON.parse(await readFile(join(this.home, 'run/runtime.json'), 'utf8')) as { pid: number; access_token: string }; }
      catch { return { pid: 0, access_token: '' }; }
    }, (v) => { if (v.pid !== this.child!.pid) return false; this.bootstrap = v.access_token; return true; });
  }
  async stop() {
    if (!this.child || this.child.exitCode !== null) return;
    const stopped = once(this.child, 'exit'); this.child.kill('SIGINT');
    await Promise.race([stopped, delay(10000).then(() => { throw new Error('Host shutdown timeout'); })]);
  }
  async request(path: string, body: unknown, token = this.bootstrap) {
    return fetch(`http://127.0.0.1:${this.port}${path}`, { method: 'POST', headers: {
      ...headers(token), 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0',
      'x-ez-host-bootstrap': this.bootstrap, 'content-type': 'application/json',
    }, body: JSON.stringify(body) });
  }
  async login(username: string) {
    const response = await this.request('/auth/login', { method: 'enterprise', username, password: username === 'admin' ? '123456' : 'test1234', native: true });
    expect(response.status).toBe(200);
    return ((await response.json()) as { token: string }).token;
  }
  async access(token: string, type: string, payload: object) {
    const response = await this.request('/host-network/access', { type, payload }, token);
    const value = await response.json() as { type: string; payload?: { access_id: string; active: boolean; items: { id: string }[] } };
    expect(response.status, JSON.stringify(value)).toBe(200);
    return value;
  }
}
const a = new Host('source', scenario.source_port), b = new Host('target', scenario.target_port);
async function device(id: string) {
  const response = await app!.inject({ url: `/api/admin/host-devices/${id}`, headers: headers(admin) });
  expect(response.statusCode).toBe(200); return response.json<HostDeviceDetail>();
}
async function manage(id: string, patch: Partial<HostDeviceState>) {
  const d = await device(id), expected = { status: d.status, name: d.name, enabled: d.enabled, users: d.users };
  const response = await app!.inject({ method: 'PATCH', url: `/api/admin/host-devices/${id}`, headers: headers(admin), payload: { expected, desired: { ...expected, ...patch } } });
  expect(response.statusCode, response.body).toBe(200);
}
async function loginCenter() {
  const response = await app!.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'admin', password: '123456' } });
  expect(response.statusCode).toBe(200);
  const login = response.json<{ token: string; center_id: string }>(); admin = login.token; center = login.center_id;
}
afterAll(async () => { await a.stop(); await b.stop(); await app?.close(); if (source.isInitialized) await source.destroy(); await pool.end(); });

it('未登记调用 Host → 正式目标用户域，身份复用、并发首次接入和定向撤销', async () => {
  expect((await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0])
    .toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'f01_m2', role: 'f01_m2', dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await upgradeDatabase(pool); await source.initialize();
  app = await createHttpApp({ source, origin }); await app.listen(scenario.center_port, '127.0.0.1'); await loginCenter();
  const issued: { token: string; llm_key: string }[] = [];
  const identities = app.get(IdentityService), prepare = identities.prepareLogin.bind(identities);
  vi.spyOn(identities, 'prepareLogin').mockImplementation(async (...args) => {
    const login = await prepare(...args); if (args[3] === 'device_user_login') issued.push(login); return login;
  });
  const modelReads = vi.spyOn(app.get(ModelsService), 'runtime');
  const ids: number[] = [];
  for (const username of ['user-a', 'user-b']) {
    const response = await app.inject({ method: 'POST', url: '/api/users', headers: headers(admin), payload: { username, display_name: username, password: 'test1234', role: 'user', enabled: true } });
    expect(response.statusCode).toBe(201); ids.push(response.json<{ id: number }>().id);
  }
  await a.start(); await b.start();
  const targetAdmin = await b.login('admin');
  const applied = await b.request('/host-network/registration', { type: 'apply', payload: { name: 'M2 目标 Host' } }, targetAdmin);
  expect(applied.status).toBe(200);
  const id = ((await applied.json()) as { registration_id: string }).registration_id;
  const grants = ids.map((user_id) => ({ user_id, desktop_view: true, desktop_control: false }));
  await manage(id, { status: 'registered', enabled: true, users: grants });
  await eventually(() => device(id), (d) => d.online);
  let userA = await a.login('user-a'); const userB = await a.login('user-b'), localB = await b.login('user-b');
  // 本机管理凭据和未经分配的管理员不能借设备在线取得业务准入。
  expect((await a.request('/host-network/access', { type: 'list_devices', payload: { offset: 0 } })).status).not.toBe(200);
  const callerAdmin = await a.login('admin');
  expect((await a.request('/host-network/access', { type: 'open_access', payload: { device_id: id, capability: 'ordinary' } }, callerAdmin)).status).not.toBe(200);
  const directory = await a.access(userA, 'list_devices', { offset: 0 });
  expect(directory.payload!.items.map((d) => d.id)).toEqual([id]);
  const registration = await a.request('/host-network/registration', { type: 'get_status' });
  expect(await registration.json()).toMatchObject({ registration_id: null, device: null });
  const open = async (token: string, capability = 'ordinary') => (await a.access(token, 'open_access', { device_id: id, capability })).payload!.access_id;
  const state = (token: string, access_id: string) => a.access(token, 'get_access', { access_id });
  const [first, second, existing] = await Promise.all([open(userA), open(userA), open(userB)]);
  expect((await state(userA, first)).payload!.active).toBe(true);
  expect((await pool.query("SELECT count(*) FROM management_audit WHERE action='device_user_login' AND actor_user_id=$1", [ids[0]])).rows[0].count).toBe('1');
  expect((await pool.query("SELECT count(*) FROM management_audit WHERE action='device_user_login' AND actor_user_id=$1", [ids[1]])).rows[0].count).toBe('0');
  expect(issued).toHaveLength(1);
  expect(modelReads.mock.calls.some(([token]) => token === issued[0]!.token)).toBe(true);
  // 用户域真实初始化写入目标自己的目录；调用端身份不产生设备登记。
  expect((await readdir(join(b.home, 'users'))).length).toBeGreaterThanOrEqual(2);
  const desktop = await open(userA, 'desktop_view');
  expect((await a.request('/host-network/access', { type: 'open_access', payload: { device_id: id, capability: 'desktop_control' } }, userA)).status).not.toBe(200);
  await manage(id, { users: grants.map((g) => ({ ...g, desktop_view: false })) });
  await eventually(() => state(userA, desktop), (v) => v.type === 'closed');
  expect((await state(userA, first)).payload!.active).toBe(true);
  await a.access(userA, 'close_access', { access_id: first });
  expect((await state(userA, second)).payload!.active).toBe(true);
  // 跨过一次真实续期，证明 Ping/Pong 之外的授权复核链路可用。
  await delay(22000);
  expect((await state(userA, second)).payload!.active).toBe(true);
  expect((await state(userB, existing)).payload!.active).toBe(true);
  await manage(id, { users: grants.filter((g) => g.user_id === ids[1]) });
  await eventually(() => state(userA, second), (v) => v.type === 'closed');
  expect((await state(userB, existing)).payload!.active).toBe(true);
  expect((await b.request('/runtime/ensure-ready', {}, localB)).status).toBe(204);
  await manage(id, { users: grants });
  const logoutAccess = await open(userA);
  expect((await a.request('/auth/logout', {}, userA)).status).toBe(200);
  userA = await a.login('user-a');
  expect((await state(userA, logoutAccess)).type).toBe('closed');
  expect((await state(userB, existing)).payload!.active).toBe(true);
  // 目标本人退出仍执行原有用户收尾，但不影响另一用户的访问。
  const other = await open(userA);
  expect((await b.request('/auth/logout', {}, localB)).status).toBe(200);
  await eventually(() => state(userB, existing), (v) => v.type === 'closed');
  expect((await state(userA, other)).payload!.active).toBe(true);
  await manage(id, { enabled: false });
  await eventually(() => state(userA, other), (v) => v.type === 'closed');
  expect((await device(id)).online).toBe(true);
  await manage(id, { enabled: true });
  const disabled = await open(userA), unaffected = await open(userB);
  const disabledReply = await app.inject({ method: 'PATCH', url: `/api/users/${ids[0]}`, headers: headers(admin), payload: { enabled: false } });
  expect(disabledReply.statusCode).toBe(200);
  expect((await a.request('/host-network/access', { type: 'get_access', payload: { access_id: disabled } }, userA)).status).not.toBe(200);
  expect((await state(userB, unaffected)).payload!.active).toBe(true);
  expect((await app.inject({ method: 'PATCH', url: `/api/users/${ids[0]}`, headers: headers(admin), payload: { enabled: true } })).statusCode).toBe(200);
  userA = await a.login('user-a');
  const restarting = await open(userA);
  await app.close(); app = undefined;
  await eventually(async () => { const r = await a.request('/host-network/access', { type: 'get_access', payload: { access_id: restarting } }, userA); return r.status; }, (status) => status !== 200);
  app = await createHttpApp({ source, origin }); await app.listen(scenario.center_port, '127.0.0.1'); await loginCenter();
  await eventually(() => device(id), (d) => d.online);
  userA = await a.login('user-a');
  const recovered = await open(userA);
  expect((await state(userA, recovered)).payload!.active).toBe(true);
  await a.access(userA, 'close_access', { access_id: recovered });
  expect((await pool.query('SELECT COUNT(*) FROM host_devices')).rows[0].count).toBe('1');
  expect((await pool.query('SELECT COUNT(*) FROM host_device_users')).rows[0].count).toBe('2');
}, 120000);
