import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import WebSocket from 'ws';
import { execFileSync, spawn, type ChildProcess } from 'node:child_process';
import { once } from 'node:events';
import { setTimeout as delay } from 'node:timers/promises';
import { afterAll, expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import type { HostDeviceDetail, HostDeviceState } from '../../dist/contracts/host-devices.js';

const smoke = process.env.CENTER_F02_SMOKE === '1';
const database = smoke ? 'f02_m3' : 'f02_m2';
const root = resolve(process.env.CENTER_F02_SCENARIO ?? '');
if (!process.env.CENTER_F02_SCENARIO || !root.startsWith(resolve(`../../.runtime-test/f02-${smoke ? 'm3' : 'm2'}-`)))
  throw new Error('必须使用全新 F02 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string;
  postgres_port: number;
  center_port: number;
  source_port: number;
  target_port: number;
};
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://${database}@127.0.0.1:${scenario.postgres_port}/${database}`;
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
  return { request_id: `m2-${type}`, command: { scope: 'runtime', payload: { type, payload } } };
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
    result: {
      payload: {
        payload: {
          workspace?: { workspace_id: string };
          session?: { session_id: string };
          sessions?: { role: string }[];
          workspaces?: unknown[];
        };
      };
    };
  };
  expect(response.status, JSON.stringify(value)).toBe(200);
  expect(value.request_id).toBe(`m2-${type}`);
  const result = value.result.payload.payload;
  // 主控会话由 Runtime 自动建立；归属断言只比较本测试创建的普通会话。
  if (type === 'list_sessions')
    result.sessions = result.sessions!.filter((s: { role: string }) => s.role !== 'controller');
  return result;
}

const sockets: WebSocket[] = [];
class Terminal {
  notices: { type: string; message?: string }[] = [];
  output = '';
  chunks = 0;
  ack = true;
  closed = false;
  pings = 0;
  constructor(readonly ws: WebSocket) {
    sockets.push(ws);
    ws.on('message', (data, binary) => {
      if (binary) {
        this.output += data.toString();
        this.chunks++;
        if (this.ack) this.control({ type: 'ack' });
      } else this.notices.push(JSON.parse(data.toString()) as { type: string });
    });
    ws.on('ping', () => this.pings++);
    ws.on('close', () => {
      this.closed = true;
    });
    ws.on('error', () => {});
  }
  control(value: object) {
    this.ws.send(JSON.stringify(value));
  }
  input(text: string) {
    this.ws.send(Buffer.from(text));
  }
  async until(check: () => boolean, ms = 10000) {
    await eventually(async () => check(), Boolean, ms);
  }
  async ready() {
    await this.until(() => this.notices.some((n) => n.type === 'created'));
    await this.until(() => /[$#%] $/.test(this.output) || this.output.includes('\x1b[?2004h'));
  }
  async pid() {
    this.input(`printf '\nM2_PID:%s\n' "$$"\n`);
    await this.until(() => /M2_PID:\d+/.test(this.output));
    return Number(this.output.match(/M2_PID:(\d+)/)![1]);
  }
  async close() {
    this.control({ type: 'close' });
    await this.until(() => this.closed);
    expect(this.notices.some((n) => n.type === 'closed')).toBe(true);
  }
}
afterAll(() => {
  for (const ws of sockets) ws.terminate();
});
const version = { 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0' };
async function socket(host: Host, device?: string, extra: Record<string, string> = {}, autoPong = true) {
  const ws = new WebSocket(
    `ws://127.0.0.1:${host.port}/user-terminals/socket${device ? `?target_device=${device}` : ''}`,
    {
      origin: `http://127.0.0.1:${host.port}`,
      headers: extra,
      autoPong,
    },
  );
  const terminal = new Terminal(ws);
  await once(ws, 'open');
  return terminal;
}
function open(token: string, origin: object) {
  return {
    type: 'open',
    bearer: token,
    client_compatibility: { version: '0.32.0', min_compatible_version: '0.32.0' },
    source: origin,
    shell: 'bash',
    size: { cols: 80, rows: 24 },
  };
}
async function gone(pid: number) {
  await eventually(async () => {
    try {
      process.kill(pid, 0);
      return false;
    } catch {
      return true;
    }
  }, Boolean);
}
it(
  smoke
    ? 'HTTP, file, SSE and terminal coexist in a short smoke check'
    : 'remote PTY uses target directories and one execution owner; ACK and cancellation survive bridging',
  async () => {
    expect(
      (
        await pool.query(
          "SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir",
        )
      ).rows[0],
    ).toEqual({
      host: '127.0.0.1',
      port: scenario.postgres_port,
      db: database,
      role: database,
      dir: join(root, 'pgdata'),
    });
    expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
    await upgradeDatabase(pool);
    await source.initialize();
    app = await createHttpApp({ source, origin });
    await app.listen(scenario.center_port, '127.0.0.1');
    await loginCenter();
    const user = await app.inject({
      method: 'POST',
      url: '/api/users',
      headers: headers(admin),
      payload: { username: 'user-a', display_name: 'user-a', password: 'test1234', role: 'user', enabled: true },
    });
    expect(user.statusCode).toBe(201);
    const userId = user.json<{ id: number }>().id;
    await a.start();
    await b.start();
    const targetAdmin = await b.login('admin');
    const applied = await b.request(
      '/host-network/registration',
      { type: 'apply', payload: { name: 'M2 target' } },
      targetAdmin,
    );
    expect(applied.status).toBe(200);
    const id = ((await applied.json()) as { registration_id: string }).registration_id;
    const grants = [{ user_id: userId, desktop_view: false, desktop_control: false }];
    await manage(id, { status: 'registered', enabled: true, users: grants });
    await eventually(
      () => device(id),
      (d) => d.online,
    );
    const token = await a.login('user-a');
    const directory = join(root, 'target-workspace');
    await mkdir(directory);
    const workspace = (
      await call(
        a,
        token,
        'register_workspace',
        { label: 'M2 remote', primary_directory: directory, additional_directories: [] },
        id,
      )
    ).workspace!;
    const session = (
      await call(a, token, 'create_session', { title: 'M2 session', workspace_id: workspace.workspace_id }, id)
    ).session!;
    const workspaceSource = { type: 'workspace', workspace_id: workspace.workspace_id };
    const sessionSource = {
      type: 'session',
      session_id: session.session_id,
      locator: { root: { type: 'workspace_primary' }, relative_path: '' },
    };
    const shells = await fetch(`http://127.0.0.1:${a.port}/user-terminals/shells?target_device=${id}`, {
      headers: { ...headers(token), ...version },
    });
    expect(shells.status).toBe(200);
    expect((await shells.json()) as unknown[]).toEqual(
      expect.arrayContaining([expect.objectContaining({ shell: 'bash' })]),
    );
    if (smoke) {
      await mixedSmoke(token, id, workspaceSource, session.session_id);
      return;
    }
    const first = await socket(a, id),
      second = await socket(a, id);
    first.control(open(token, workspaceSource));
    second.control(open(token, sessionSource));
    await first.ready();
    await second.ready();
    const firstPid = await first.pid(),
      secondPid = await second.pid();
    const ppid = (pid: number) =>
      Number(execFileSync('ps', ['-o', 'ppid=', '-p', String(pid)], { encoding: 'utf8' }).trim());
    expect(ppid(firstPid)).toBe(b.child!.pid);
    expect(ppid(secondPid)).toBe(b.child!.pid);
    first.input('printf \'\nM2_CWD:%s\n\' "$PWD"\n');
    await first.until(() => first.output.includes(`M2_CWD:${directory}`));
    second.control({ type: 'resize', size: { cols: 101, rows: 37 } });
    second.input('stty size\n');
    await second.until(() => second.output.includes('37 101'));
    // 终端 Ping/Pong 贯穿业务流，与中心控制心跳独立。
    await second.until(() => second.pings > 0, 15000);
    second.input("printf 'M2_%s\n' ALIVE\n");
    await second.until(() => second.output.includes('M2_ALIVE'));
    await first.close();
    await gone(firstPid);
    second.input("printf 'M2_%s\n' INDEPENDENT\n");
    await second.until(() => second.output.includes('M2_INDEPENDENT'));
    // 暂停 UI ACK，即使中心已归还传输信用，也只能收到一块 PTY 输出。
    second.ack = false;
    const chunks = second.chunks;
    second.input("head -c 262144 /dev/zero | tr '\\0' X; printf 'M2_%s\n' DRAINED\n");
    await second.until(() => second.chunks > chunks);
    await delay(250);
    expect(second.chunks - chunks).toBe(1);
    second.ack = true;
    second.control({ type: 'ack' });
    await second.until(() => second.output.includes('M2_DRAINED'));
    expect(second.notices.filter((n) => n.type === 'input_ack').length).toBeGreaterThanOrEqual(4);
    await second.close();
    await gone(secondPid);
    for (const [body, extra, expected] of [
      [{ type: 'ack' }, {}, 'error'],
      [{ ...open(token, workspaceSource), bearer: 'invalid' }, {}, 'error'],
      [{ ...open(token, workspaceSource), client_compatibility: null }, {}, 'compatibility_error'],
      [open(token, workspaceSource), { ...version, 'x-ez-client-version': '99.0.0' }, 'compatibility_error'],
      [open(token, { type: 'workspace', workspace_id: 'missing' }), {}, 'error'],
    ] as const) {
      const invalid = await socket(a, id, extra);
      invalid.control(body);
      await invalid.until(() => invalid.closed);
      expect(invalid.notices.some((n) => n.type === expected)).toBe(true);
    }
    const idle = await socket(a, id);
    await idle.until(() => idle.closed, 8000);
    expect(idle.notices.some((n) => n.type === 'error')).toBe(true);
    const revoked = await socket(a, id);
    revoked.control(open(token, workspaceSource));
    await revoked.ready();
    const revokedPid = await revoked.pid();
    await manage(id, { users: [] });
    await revoked.until(() => revoked.closed);
    await gone(revokedPid);
    await manage(id, { users: grants });
    const disconnected = await socket(a, id);
    disconnected.control(open(token, workspaceSource));
    await disconnected.ready();
    const disconnectedPid = await disconnected.pid();
    disconnected.ws.terminate();
    await gone(disconnectedPid);
    // 创建中立即断线：无需收到 Created，也必须由目标 owner 收齐启动结果。
    for (let index = 0; index < 8; index++) {
      const racing = await socket(a, id);
      await new Promise<void>((resolve, reject) =>
        racing.ws.send(JSON.stringify(open(token, workspaceSource)), (error) => (error ? reject(error) : resolve())),
      );
      racing.ws.terminate();
    }
    const children = () =>
      execFileSync('ps', ['-axo', 'pid=,ppid='], { encoding: 'utf8' })
        .trim()
        .split('\n')
        .filter((line) => Number(line.trim().split(/\s+/)[1]) === b.child!.pid);
    await delay(500);
    await eventually(
      async () => children(),
      (value) => value.length === 0,
    );
    // 客户端不回答业务 Ping；中心连接仍在线，不能代答使 PTY 无限保活。
    const silent = await socket(a, id, {}, false);
    silent.control(open(token, workspaceSource));
    await silent.ready();
    const silentPid = await silent.pid();
    await silent.until(() => silent.closed, 35000);
    await gone(silentPid);
    expect(silent.pings).toBeGreaterThanOrEqual(2);
    const localToken = await b.login('user-a');
    const local = await socket(b);
    local.control(open(localToken, workspaceSource));
    await local.ready();
    const localPid = await local.pid();
    expect(ppid(localPid)).toBe(b.child!.pid);
    await local.close();
    await gone(localPid);
    const lost = await socket(a, id);
    lost.control(open(token, workspaceSource));
    await lost.ready();
    const lostPid = await lost.pid();
    await a.stop();
    await gone(lostPid);
    expect(b.child!.exitCode).toBeNull();
    const db = await pool.query(
      'SELECT (SELECT COUNT(*) FROM users)::int AS users,(SELECT COUNT(*) FROM host_devices)::int AS devices,(SELECT COUNT(*) FROM host_device_users)::int AS grants',
    );
    expect(db.rows[0]).toEqual({ users: 2, devices: 1, grants: 1 });
    expect((await call(b, localToken, 'list_sessions')).sessions).toHaveLength(1);
  },
  120000,
);

// M3 仅复用启动夹具做短组合检查，不重复执行上面的 M2 回归矩阵。
async function mixedSmoke(token: string, id: string, workspaceSource: object, session: string) {
  const remoteHeaders = { ...version, ...headers(token), 'x-ez-target-device': id };
  const base = `http://127.0.0.1:${a.port}`;
  const abort = new AbortController();
  const deadline = setTimeout(() => abort.abort(), 15000);
  const terminal = await socket(a, id);
  terminal.control(open(token, workspaceSource));
  try {
    await terminal.ready();
    const response = await fetch(`${base}/events`, { headers: remoteHeaders, signal: abort.signal });
    expect(response.status).toBe(200);
    const reader = response.body!.getReader();
    expect(new TextDecoder().decode((await reader.read()).value)).toContain('connected');
    const bytes = Buffer.alloc(1024 * 1024, 'F02 smoke\n');
    const form = new FormData();
    form.append('file', new Blob([new Uint8Array(bytes)]), 'smoke.txt');
    terminal.input("printf 'F02_%s\n' SMOKE_OK\n");
    const [uploaded] = await Promise.all([
      fetch(`${base}/sessions/${session}/attachments`, { method: 'POST', headers: remoteHeaders, body: form }),
      call(a, token, 'rename_session', { session_id: session, title: 'F02 smoke renamed' }, id),
      terminal.until(() => terminal.output.includes('F02_SMOKE_OK')),
    ]);
    expect(uploaded.status).toBe(200);
    const attachment = ((await uploaded.json()) as { attachment: { attachment_id: string } }).attachment;
    const download = await fetch(`${base}/sessions/${session}/attachments/${attachment.attachment_id}/download`, {
      headers: remoteHeaders,
    });
    expect(download.status).toBe(200);
    expect(Buffer.from(await download.arrayBuffer())).toEqual(bytes);
    let events = '';
    while (!events.includes('session_changed')) {
      const next = await reader.read();
      expect(next.done).toBe(false);
      events += new TextDecoder().decode(next.value);
    }
    expect((await device(id)).online).toBe(true);
    expect((await call(a, token, 'list_workspaces')).workspaces).toEqual([]);
    expect((await call(a, token, 'list_sessions')).sessions).toEqual([]);
    const pid = await terminal.pid();
    await terminal.close();
    await gone(pid);
  } finally {
    clearTimeout(deadline);
    abort.abort();
    terminal.ws.terminate();
  }
}
