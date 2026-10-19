import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { spawn, type ChildProcess } from 'node:child_process';
import { randomUUID, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { createServer as createTlsServer } from 'node:tls';
import { connect as connectTcp, type Socket } from 'node:net';
import { setTimeout as delay } from 'node:timers/promises';
import { WebSocket } from 'ws';
import { afterAll, expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import type { HostDeviceDetail } from '../../dist/contracts/host-devices.js';

const root = resolve(process.env.CENTER_F01_SCENARIO ?? '');
if (!process.env.CENTER_F01_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f01-m1-'))) throw new Error('必须使用全新 M1 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as { root: string; postgres_port: number; center_port: number; host_port: number; tls_port: number };
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://f01_m1@127.0.0.1:${scenario.postgres_port}/f01_m1`;
const pool = createPool({ url }), source = createDataSource({ url });
const origin = `http://127.0.0.1:${scenario.center_port}`;
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
let child: ChildProcess | undefined;
let admin = '', center = '', bootstrap = '';
let tls: ReturnType<typeof createTlsServer> | undefined;
const sockets = new Set<Socket>();
const clients = new Set<WebSocket>();
const home = join(root, 'host');
const hostOrigin = `http://127.0.0.1:${scenario.host_port}`;
const version = { 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0' };
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
async function eventually<T>(read: () => Promise<T>, check: (value: T) => boolean, ms = 12000): Promise<T> {
  const until = Date.now() + ms;
  let value: T;
  do { value = await read(); if (check(value)) return value; await delay(100); } while (Date.now() < until);
  throw new Error(`state did not converge: ${JSON.stringify(value!)}`);
}
async function startCenter() {
  app = await createHttpApp({ source, origin });
  await app.listen(scenario.center_port, '127.0.0.1');
  const login = await app.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'admin', password: '123456' } });
  expect(login.statusCode).toBe(200);
  const value = login.json<{ token: string; center_id: string }>();
  admin = value.token; center = value.center_id;
}
async function config(centerUrl = origin) {
  await writeFile(join(home, 'host.toml'), `version = "0.27.0"\nmode = "enterprise"\n[enterprise]\ncenter_url = "${centerUrl}"\ncenter_id = "${center}"\n[host_access]\nport = ${scenario.host_port}\nremote_enabled = false\nscheme = "http"\nserver_names = []\n`, { mode: 0o600 });
}
async function startHost(cert?: string) {
  child = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', home], {
    stdio: 'ignore', env: { ...process.env, ...(cert ? { SSL_CERT_FILE: cert } : {}) },
  });
  await eventually(async () => {
    if (child!.exitCode !== null) throw new Error('Host exited');
    try { return JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8')) as { pid: number; access_token: string }; }
    catch { return { pid: 0, access_token: '' }; }
  }, (value) => { if (value.pid !== child!.pid) return false; bootstrap = value.access_token; return true; });
}
async function stopHost() {
  if (!child || child.exitCode !== null) return;
  const stopped = once(child, 'exit'); child.kill('SIGINT');
  await Promise.race([stopped, delay(10000).then(() => { throw new Error('Host shutdown timeout'); })]);
}
async function local(path: string, body: unknown, token = bootstrap) {
  return fetch(`${hostOrigin}${path}`, { method: 'POST', headers: { ...version, ...headers(token), 'x-ez-host-bootstrap': bootstrap, 'content-type': 'application/json' }, body: JSON.stringify(body) });
}
type Status = { connection: string; intent: string; registration_id: string; device: { status: string; online: boolean } | null; error: unknown };
async function command(type = 'get_status', token = bootstrap) {
  const reply = await local('/host-network/registration', { type, ...(type === 'apply' ? { payload: { name: 'M1 正式 Host' } } : {}) }, token);
  expect(reply.status).toBe(200);
  return await reply.json() as Status;
}
async function device(id: string) {
  const reply = await app!.inject({ method: 'GET', url: `/api/admin/host-devices/${id}`, headers: headers(admin) });
  expect(reply.statusCode).toBe(200);
  return reply.json<HostDeviceDetail>();
}
async function manage(id: string, patch: Partial<Pick<HostDeviceDetail, 'status' | 'enabled'>>) {
  const d = await device(id);
  const expected = { status: d.status, name: d.name, enabled: d.enabled, users: d.users };
  const reply = await app!.inject({ method: 'PATCH', url: `/api/admin/host-devices/${id}`, headers: headers(admin), payload: { expected, desired: { ...expected, ...patch } } });
  expect(reply.statusCode).toBe(200);
}
function socket(id: string, secret: string, autoPong = true) {
  const ws = new WebSocket(`${origin.replace('http', 'ws')}/api/host-network/connect`, { headers: { ...headers(secret), 'x-ez-device-id': id }, autoPong });
  clients.add(ws); ws.on('error', () => {}); ws.on('close', () => clients.delete(ws));
  return ws;
}
async function rejected(id: string, secret: string, status: number) {
  const ws = socket(id, secret);
  const [request, response] = await once(ws, 'unexpected-response');
  expect(response.statusCode).toBe(status);
  request.destroy(); ws.terminate();
}
async function connectDevice(id: string, secret: string, autoPong = true) {
  const ws = socket(id, secret, autoPong);
  const [raw] = await once(ws, 'message');
  const hello = JSON.parse(raw.toString());
  expect(hello).toMatchObject({ type: 'hello', protocol_version: 2, center_id: center, registration_id: id });
  ws.send(JSON.stringify({ ...hello, type: 'ready' }));
  await once(ws, 'message');
  return ws;
}
afterAll(async () => {
  await stopHost();
  for (const ws of clients) ws.terminate();
  for (const socket of sockets) socket.destroy();
  if (tls) await new Promise<void>((resolve) => tls!.close(() => resolve()));
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
});

it('正式 Host：登记、在线、停用、重启、冲突、静默断线、注销及 WSS', async () => {
  const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0];
  expect(actual).toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'f01_m1', role: 'f01_m1', dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await upgradeDatabase(pool); await source.initialize(); await startCenter();
  await mkdir(home, { mode: 0o700 }); await config(); await startHost();
  const login = await local('/auth/login', { method: 'enterprise', username: 'admin', password: '123456', native: true });
  expect(login.status).toBe(200);
  const user = ((await login.json()) as { token: string }).token;
  const applied = await command('apply', user), id = applied.registration_id;
  await eventually(() => device(id), (d) => d.online);
  expect((await command()).connection).toBe('online');
  // 在线不依赖用户登录或 UI；停用也保留控制连接。
  expect((await local('/auth/logout', {}, user)).status).toBe(200);
  await manage(id, { status: 'registered', enabled: false });
  expect((await device(id)).online).toBe(true);
  const saved = JSON.parse(await readFile(join(home, 'network/identity.json'), 'utf8')) as { current: { secret: string } };
  await rejected(id, saved.current.secret, 409);
  await rejected(id, '0'.repeat(64), 401);
  expect((await device(id)).online).toBe(true);
  const events = new AbortController();
  const eventResponse = await fetch(`${hostOrigin}/host-network/events`, { headers: { ...version, ...headers(bootstrap), 'x-ez-host-bootstrap': bootstrap }, signal: events.signal });
  expect(eventResponse.status).toBe(200);
  expect(new TextDecoder().decode((await eventResponse.body!.getReader().read()).value)).toContain('host_network_changed');
  events.abort();
  // 中心重启：在线表为空，Host 重新证明身份，不依赖用户重新登录。
  await app!.close(); app = undefined;
  const offline = await command(); expect(offline.connection).not.toBe('online');
  await startCenter();
  await eventually(() => device(id), (d) => d.online, 20000);
  // 已确认身份的 WSS：隔离 TLS 网关，仅设置测试 CA，不关闭证书校验。
  await stopHost();
  await eventually(() => device(id), (d) => !d.online);
  tls = createTlsServer({ key: await readFile(join(root, 'tls.key')), cert: await readFile(join(root, 'tls.crt')) }, (incoming) => {
    const outgoing = connectTcp(scenario.center_port, '127.0.0.1');
    for (const s of [incoming, outgoing]) { sockets.add(s); s.on('close', () => sockets.delete(s)); s.on('error', () => { incoming.destroy(); outgoing.destroy(); }); }
    incoming.pipe(outgoing).pipe(incoming);
    incoming.on('close', () => outgoing.destroy()); outgoing.on('close', () => incoming.destroy());
  });
  await new Promise<void>((resolve) => tls!.listen(scenario.tls_port, '127.0.0.1', resolve));
  await config(`https://localhost:${scenario.tls_port}`);
  await startHost(); // 未信任证书不得在线。
  await delay(1500); expect((await device(id)).online).toBe(false); await stopHost();
  await startHost(join(root, 'ca.crt'));
  await eventually(() => device(id), (d) => d.online, 20000);
  await stopHost();
  await eventually(() => device(id), (d) => !d.online);
  await config(); await startHost();
  await eventually(() => device(id), (d) => d.online);
  await manage(id, { status: 'revoked' });
  // 不调用本机 get_status 触发恢复，直接验证连接通知后的私有意图。
  await eventually(async () => JSON.parse(await readFile(join(home, 'network/identity.json'), 'utf8')) as { current: { intent: string } }, (v) => v.current.intent === 'closed');
  expect((await device(id)).online).toBe(false);
  expect((await command()).connection).toBe('disconnected');
  await rejected(id, saved.current.secret, 410);
  // 中心离线时退出，Host 重启后仍自动恢复注销，不需要用户再次登录或查询。
  const again = await local('/auth/login', { method: 'enterprise', username: 'admin', password: '123456', native: true });
  expect(again.status).toBe(200);
  const renewed = await command('apply', ((await again.json()) as { token: string }).token);
  await eventually(() => device(renewed.registration_id), (d) => d.online);
  await app!.close(); app = undefined;
  expect((await command('exit')).intent).toBe('exit');
  await stopHost(); await startHost(); await startCenter();
  await eventually(async () => JSON.parse(await readFile(join(home, 'network/identity.json'), 'utf8')) as { current: { intent: string } }, (v) => v.current.intent === 'closed', 40000);
  expect((await device(renewed.registration_id)).status).toBe('revoked');
  expect((await device(renewed.registration_id)).online).toBe(false);
  await stopHost();
  // 独立设备验证真实未响应 Pong 的 45 秒期限，并检查重连不受旧 close 影响。
  const probe = { center_id: center, id: randomUUID(), installation_id: randomUUID(), secret: randomBytes(32).toString('hex'), name: '心跳测试', platform: 'test', host_version: '0.31.0' };
  expect((await app!.inject({ method: 'POST', url: '/api/host-devices', headers: headers(admin), payload: probe })).statusCode).toBe(200);
  const silent = await connectDevice(probe.id, probe.secret, false);
  expect((await device(probe.id)).online).toBe(true);
  const beforeHeartbeat = await device(probe.id);
  const began = Date.now(); await once(silent, 'close');
  expect(Date.now() - began).toBeGreaterThanOrEqual(44000);
  expect((await device(probe.id)).online).toBe(false);
  expect((await device(probe.id)).updated_at).toBe(beforeHeartbeat.updated_at);
  const alive = await connectDevice(probe.id, probe.secret);
  await delay(100); expect((await device(probe.id)).online).toBe(true);
  alive.terminate(); await eventually(() => device(probe.id), (d) => !d.online);
  const rows = (await pool.query('SELECT status, COUNT(*)::int AS count FROM host_devices GROUP BY status ORDER BY status')).rows;
  expect(rows).toEqual([{ status: 'pending', count: 1 }, { status: 'revoked', count: 2 }]);
  expect((await pool.query('SELECT COUNT(*) FROM host_device_users')).rows[0].count).toBe('0');
  // 心跳仅改变内存，不修改登记时间或授权事实。
  console.log('M1 isolated tables', rows, 'grants=0');
}, 180000);
