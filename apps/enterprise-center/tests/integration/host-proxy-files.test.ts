import { readFile, mkdir, writeFile, readdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
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
if (!process.env.CENTER_F02_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f02-m1-')))
  throw new Error('必须使用全新 M1 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string;
  postgres_port: number;
  center_port: number;
  source_port: number;
  target_port: number;
};
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://f02_m1@127.0.0.1:${scenario.postgres_port}/f02_m1`;
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
  upstream.closeAllConnections();
  await new Promise<void>((done) => upstream.close(() => done()));
  await a.stop();
  await b.stop();
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
});

function command(type: string, payload: object = {}) {
  return { request_id: `m1-${type}`, command: { scope: 'runtime', payload: { type, payload } } };
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
          run?: { run_id: string; status: string };
          tasks?: { child_task_id: string; status: string }[];
          snapshot?: {
            value: {
              items?: {
                message_id: string;
                segments?: { type: string; tools?: { tool_name: string; call_id: string }[] }[];
              }[];
              files?: { resource_ref_id: string }[];
            };
          };
          workspace?: { workspace_id: string };
          session?: { session_id: string };
          sessions?: { role: string }[];
          workspaces?: unknown[];
        };
      };
    };
  };
  expect(response.status, JSON.stringify(value)).toBe(200);
  expect(value.request_id).toBe(`m1-${type}`);
  const result = value.result.payload.payload;
  // 主控会话由 Runtime 自动建立；归属断言只比较本测试创建的普通会话。
  if (type === 'list_sessions')
    result.sessions = result.sessions!.filter((s: { role: string }) => s.role !== 'controller');
  return result;
}

it('文件与 SSE 复用目标本人 Runtime，传输与取消不在本机落盘', async () => {
  expect(
    (
      await pool.query(
        "SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir",
      )
    ).rows[0],
  ).toEqual({
    host: '127.0.0.1',
    port: scenario.postgres_port,
    db: 'f02_m1',
    role: 'f02_m1',
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
  await configureModel();
  await a.start();
  await b.start();
  const targetAdmin = await b.login('admin');
  const applied = await b.request(
    '/host-network/registration',
    { type: 'apply', payload: { name: 'M1 target' } },
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
  const token = await a.login('user-a');
  const targetToken = await b.login('user-a');
  const version = { 'x-ez-client-version': '0.32.0', 'x-ez-min-compatible-version': '0.32.0' };
  const remoteHeaders = { ...version, ...headers(token), 'x-ez-target-device': id };
  const base = `http://127.0.0.1:${a.port}`;
  const request = (path: string, init: RequestInit = {}) =>
    fetch(`${base}${path}`, {
      ...init,
      headers: { ...remoteHeaders, ...init.headers },
    });
  const preflight = await fetch(`${base}/sessions/fixture/attachments/fixture/preview`, {
    method: 'OPTIONS',
    headers: {
      origin: 'http://client.example',
      'access-control-request-method': 'GET',
      'access-control-request-headers': 'authorization,x-ez-target-device,range,if-range',
    },
  });
  expect(preflight.status).toBe(204);
  expect(preflight.headers.get('access-control-expose-headers')).toContain('content-range');
  const created = (await call(a, token, 'create_session', { title: 'M1 attachment target' }, id)).session!;
  const session = created.session_id;
  // 明显超过 64 KiB 信用窗口；不制造巨型文件或额外构建缓存。
  const bytes = Buffer.alloc(2 * 1024 * 1024, 'remote bytes\n');
  const png = Buffer.from(
    'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC',
    'base64',
  );
  async function upload(name: string, data: Buffer) {
    const form = new FormData();
    form.append('file', new Blob([new Uint8Array(data)]), name);
    const response = await request(`/sessions/${session}/attachments`, { method: 'POST', body: form });
    const result = (await response.json()) as { attachment: { attachment_id: string; size_bytes: number } };
    expect(response.status, JSON.stringify(result)).toBe(200);
    expect(result.attachment.size_bytes).toBe(data.length);
    return result.attachment.attachment_id;
  }
  const attachment = await upload('远端文件.txt', bytes);
  const image = await upload('image.png', png);
  const attachmentUrl = `/sessions/${session}/attachments/${attachment}`;
  for (const path of [`${attachmentUrl}/download`, `${attachmentUrl}/preview`]) {
    const response = await request(path);
    expect(response.status).toBe(200);
    expect(Buffer.from(await response.arrayBuffer())).toEqual(bytes);
    expect(response.headers.get('cache-control')).toContain('no-store');
  }
  const range = await request(`${attachmentUrl}/preview`, { headers: { range: 'bytes=4-99' } });
  expect(range.status).toBe(206);
  expect(range.headers.get('content-range')).toBe(`bytes 4-99/${bytes.length}`);
  expect(Buffer.from(await range.arrayBuffer())).toEqual(bytes.subarray(4, 100));
  for (const mode of ['preview', 'thumbnail']) {
    const response = await request(`/sessions/${session}/attachments/${image}/${mode}`);
    expect(response.status).toBe(200);
    expect(response.headers.get('content-type')).toMatch(/^image\//);
    expect((await response.arrayBuffer()).byteLength).toBeGreaterThan(0);
  }
  expect((await request(`/sessions/${session}/export.md`)).status).toBe(200);
  // 目录/文件沿原 JSON 接口，路径只由目标解释。
  const folder = join(root, 'target-workspace');
  await mkdir(folder);
  await writeFile(join(folder, 'remote.txt'), bytes);
  await writeFile(join(folder, 'empty.txt'), '');
  for (const [operation, body] of [
    ['list', { path: folder, include_hidden: false }],
    ['select-directory', { path: folder }],
    ['preview', { path: join(folder, 'remote.txt') }],
  ] as const) {
    const response = await request(`/host-files/${operation}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    expect(response.status, await response.text()).toBe(200);
  }
  for (const name of ['remote.txt', 'empty.txt']) {
    const response = await request('/host-files/download', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ path: join(folder, name) }),
    });
    expect(response.status).toBe(200);
    expect(Buffer.from(await response.arrayBuffer())).toEqual(name === 'empty.txt' ? Buffer.alloc(0) : bytes);
  }
  const workspace = (
    await call(
      a,
      token,
      'register_workspace',
      { label: 'M1 files', primary_directory: folder, additional_directories: [] },
      id,
    )
  ).workspace!;
  const projectSession = (
    await call(a, token, 'create_session', { title: 'M1 project files', workspace_id: workspace.workspace_id }, id)
  ).session!.session_id;
  for (const operation of ['list', 'stat', 'preview', 'download']) {
    const locator = { root: { type: 'workspace_primary' }, relative_path: operation === 'list' ? '' : 'remote.txt' };
    const response = await request(`/sessions/${projectSession}/resource-files/${operation}`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ locator }),
    });
    expect(response.status).toBe(200);
    if (operation === 'download') expect(Buffer.from(await response.arrayBuffer())).toEqual(bytes);
    else await response.json();
  }
  // 工具/子任务资源也必须走原授权解析，不能变成任意文件读取或路由拒绝。
  for (const prefix of [
    `/sessions/${session}/messages/m/resources/r`,
    `/sessions/${session}/child-tasks/c/messages/m/resources/r`,
  ]) {
    for (const operation of ['download', 'preview']) {
      const response = await request(`${prefix}/${operation}`);
      const failure = (await response.json()) as { error: { code: string } };
      expect(response.status).toBeGreaterThanOrEqual(400);
      expect(failure.error.code).not.toMatch(/^network_/);
    }
  }
  // 由可控 Provider 真正执行 read_image / delegate_task，验证主会话和子任务资源字节。
  await writeFile(join(folder, 'fixture.png'), png);
  await call(a, token, 'set_session_approval_mode', { session_id: projectSession, approval_mode: 'auto' }, id);
  const observing = new AbortController();
  const observed = await request('/events', { signal: observing.signal });
  await observed.body!.getReader().read();
  const mainRun = (
    await call(
      a,
      token,
      'submit_input',
      { session_id: projectSession, message: 'M1_READ_IMAGE', variant: 'build', idempotency_key: 'm1-tool-image' },
      id,
    )
  ).run!;
  observing.abort();
  const terminal = await eventually(
    () => call(a, token, 'get_run', { session_id: projectSession, run_id: mainRun.run_id }, id),
    (value) => ['completed', 'failed', 'cancelled'].includes(value.run!.status),
  );
  expect(terminal.run!.status, JSON.stringify(terminal)).toBe('completed');
  async function verifyToolImage(child?: string) {
    const owner = child
      ? { type: 'child_task', session_id: projectSession, child_task_id: child }
      : { type: 'main_session', session_id: projectSession };
    const page = (await call(a, token, 'list_conversation_page', { owner, limit: 100 }, id)).snapshot!.value;
    const item = page.items!.find((item) =>
      item.segments?.some((segment) => segment.tools?.some((tool) => tool.tool_name === 'read_image')),
    )!;
    expect(item).toBeDefined();
    const tool = item
      .segments!.flatMap((segment) => segment.tools ?? [])
      .find((tool) => tool.tool_name === 'read_image')!;
    const detail = (
      await call(a, token, 'get_tool_detail', { owner, message_id: item.message_id, call_id: tool.call_id }, id)
    ).snapshot!.value;
    expect(detail.files).toHaveLength(1);
    const prefix = child ? `/sessions/${projectSession}/child-tasks/${child}` : `/sessions/${projectSession}`;
    for (const operation of ['download', 'preview']) {
      const response = await request(
        `${prefix}/messages/${item.message_id}/resources/${detail.files![0]!.resource_ref_id}/${operation}`,
      );
      expect(response.status, await response.clone().text()).toBe(200);
      expect(Buffer.from(await response.arrayBuffer())).toEqual(png);
    }
  }
  await verifyToolImage();
  const parent = (
    await call(
      a,
      token,
      'submit_input',
      { session_id: projectSession, message: 'M1_DELEGATE', variant: 'build', idempotency_key: 'm1-child-image' },
      id,
    )
  ).run!;
  const children = await eventually(
    () => call(a, token, 'list_child_tasks', { session_id: projectSession, parent_run_id: parent.run_id }, id),
    (value) => value.tasks?.some((task) => task.status === 'completed') ?? false,
  );
  await verifyToolImage(children.tasks![0]!.child_task_id);
  // 首次发送仅带附件，原 key 可回读已提交结果；模型服务为本测试 loopback 夹具。
  const manifest = {
    idempotency_key: 'm1-files-first',
    variant: 'build',
    approval_mode: 'auto',
    mode: 'normal',
    message: '',
    attachments: [
      { selection_key: 'text', original_name: 'first.txt', size_bytes: bytes.length },
      { selection_key: 'image', original_name: 'first.png', size_bytes: png.length },
    ],
  };
  const first = new FormData();
  first.append('manifest', JSON.stringify(manifest));
  first.append('text', new Blob([bytes]), 'first.txt');
  first.append('image', new Blob([png]), 'first.png');
  const materialized = await request('/session-materializations', { method: 'POST', body: first });
  const materialization = (await materialized.json()) as {
    session: { session_id: string };
    input_id: string;
    attachments: { attachment_id: string }[];
  };
  expect(materialized.status, JSON.stringify(materialization)).toBe(200);
  expect(materialization.attachments).toHaveLength(2);
  expect(
    await call(a, token, 'get_session_materialization', { idempotency_key: manifest.idempotency_key }, id),
  ).toMatchObject({ materialization: { input_id: materialization.input_id } });
  // SSE 取消只停止订阅，重连后仍读取同一目标快照。
  const sseAbort = new AbortController();
  const events = await request('/events', { signal: sseAbort.signal });
  expect(events.headers.get('content-type')).toContain('text/event-stream');
  const reader = events.body!.getReader();
  expect(new TextDecoder().decode((await reader.read()).value)).toContain('connected');
  await call(a, token, 'rename_session', { session_id: session, title: 'M1 event arrived' }, id);
  const received = await Promise.race([
    reader.read(),
    delay(5000).then(() => {
      throw new Error('event timeout');
    }),
  ]);
  expect(new TextDecoder().decode(received.value)).toContain('runtime_event');
  sseAbort.abort();
  await reader.cancel().catch(() => {});
  expect((await call(b, targetToken, 'get_session', { session_id: session })).session).toMatchObject({
    title: 'M1 event arrived',
  });
  const reconnect = new AbortController();
  const again = await request('/events', { signal: reconnect.signal });
  expect(new TextDecoder().decode((await again.body!.getReader().read()).value)).toContain('connected');
  reconnect.abort();
  // 浏览器媒体沿当前 Cookie 的不可变版本声明；旧账号上下文拒绝。
  const web = await a.request(
    '/auth/login',
    { method: 'enterprise', username: 'user-a', password: 'test1234', native: false },
    a.bootstrap,
    { origin: base },
  );
  expect(web.status).toBe(200);
  const cookie = web.headers.get('set-cookie')!.split(';')[0]!;
  const login = (await web.json()) as { login_context: string };
  const media = await fetch(
    `${base}${attachmentUrl}/download?target_device=${id}&login_context=${encodeURIComponent(login.login_context)}`,
    { headers: { cookie } },
  );
  expect(media.status, await media.clone().text()).toBe(200);
  expect(Buffer.from(await media.arrayBuffer())).toEqual(bytes);
  expect(
    (
      await fetch(`${base}${attachmentUrl}/preview?target_device=${id}&login_context=old-context`, {
        headers: { cookie },
      })
    ).status,
  ).toBe(409);
  const switched = await a.request(
    '/auth/login',
    { method: 'enterprise', username: 'user-b', password: 'test1234', native: false },
    a.bootstrap,
    { origin: base, cookie },
  );
  expect(switched.status).toBe(200);
  const nextCookie = switched.headers.get('set-cookie')!.split(';')[0]!;
  expect(
    (
      await fetch(
        `${base}${attachmentUrl}/preview?target_device=${id}&login_context=${encodeURIComponent(login.login_context)}`,
        { headers: { cookie: nextCookie } },
      )
    ).status,
  ).toBe(409);
  // 截断或取消 multipart，已提交附件仍在，未提交 staging 必须归零。
  const users = await readdir(join(b.home, 'users'));
  const domain = users.find((name) => name.endsWith(`_${ids[0]}`))!;
  const staging = join(b.home, 'users', domain, 'data/staging/uploads');
  const partial =
    '--m1\r\nContent-Disposition: form-data; name="file"; filename="cancel.txt"\r\nContent-Type: text/plain\r\n\r\n' +
    'x'.repeat(128 * 1024);
  const truncated = await request(`/sessions/${session}/attachments`, {
    method: 'POST',
    headers: { 'content-type': 'multipart/form-data; boundary=m1' },
    body: partial,
  });
  expect(truncated.status).toBe(400);
  await eventually(
    () => readdir(staging),
    (files) => files.length === 0,
  );
  const cancel = new AbortController();
  const pending = request(`/sessions/${session}/attachments`, {
    method: 'POST',
    headers: { 'content-type': 'multipart/form-data; boundary=m1' },
    signal: cancel.signal,
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(partial));
      },
    }),
    duplex: 'half',
  } as RequestInit);
  await eventually(
    () => readdir(staging),
    (files) => files.length === 1,
  );
  cancel.abort();
  await pending.catch(() => {});
  await eventually(
    () => readdir(staging),
    (files) => files.length === 0,
  );
  expect(Buffer.from(await (await request(`${attachmentUrl}/download`)).arrayBuffer())).toEqual(bytes);
  expect((await call(a, token, 'list_sessions')).sessions).toEqual([]);
  expect((await call(a, token, 'list_workspaces')).workspaces).toEqual([]);
  // 目标提前返回业务拒绝时，不等待永远不结束的上传正文。
  const earlyAbort = new AbortController();
  const early = await request('/sessions/missing/attachments', {
    method: 'POST',
    headers: { 'content-type': 'multipart/form-data; boundary=m1' },
    signal: earlyAbort.signal,
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(partial));
      },
    }),
    duplex: 'half',
  } as RequestInit);
  expect(early.status).toBe(404);
  await early.text();
  earlyAbort.abort();
  // 已接收多个文件时取消，所有 staging 均清理，原有附件保留。
  const multiManifest = {
    ...manifest,
    idempotency_key: 'm1-cancel-multi',
    attachments: [
      { selection_key: 'one', original_name: 'one.txt', size_bytes: 3 },
      { selection_key: 'two', original_name: 'two.txt', size_bytes: 999999 },
    ],
  };
  const multiBody =
    '--m1\r\nContent-Disposition: form-data; name="manifest"\r\n\r\n' +
    JSON.stringify(multiManifest) +
    '\r\n--m1\r\nContent-Disposition: form-data; name="one"; filename="one.txt"\r\n\r\none' +
    '\r\n--m1\r\nContent-Disposition: form-data; name="two"; filename="two.txt"\r\n\r\n' +
    'x'.repeat(128 * 1024);
  const multiAbort = new AbortController();
  const multi = request('/session-materializations', {
    method: 'POST',
    headers: { 'content-type': 'multipart/form-data; boundary=m1' },
    signal: multiAbort.signal,
    body: new ReadableStream({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(multiBody));
      },
    }),
    duplex: 'half',
  } as RequestInit);
  await eventually(
    () => readdir(staging),
    (files) => files.length === 2,
  );
  multiAbort.abort();
  await multi.catch(() => {});
  await eventually(
    () => readdir(staging),
    (files) => files.length === 0,
  );
  expect(
    await call(a, token, 'get_session_materialization', { idempotency_key: multiManifest.idempotency_key }, id),
  ).toMatchObject({ materialization: null });
  // 下载字节已到齐但 FIN 丢失时，不得报完整成功；恢复后同一附件仍可读取。
  const relay = NetworkStreams.prototype.binary;
  let cut = false;
  const lostFin = vi.spyOn(NetworkStreams.prototype, 'binary').mockImplementation(function (
    this: NetworkStreams,
    peer,
    data,
  ) {
    if (!cut && peer.id === id && decodeFrame(data).kind === 4) {
      cut = true;
      peer.stop();
      return;
    }
    return relay.call(this, peer, data);
  });
  try {
    await expect(
      (async () => {
        const response = await request(`${attachmentUrl}/download`);
        expect(response.status).toBe(200);
        await response.arrayBuffer();
      })(),
    ).rejects.toThrow();
    expect(cut).toBe(true);
  } finally {
    lostFin.mockRestore();
  }
  await eventually(
    () => device(id),
    (d) => d.online,
  );
  expect(Buffer.from(await (await request(`${attachmentUrl}/download`)).arrayBuffer())).toEqual(bytes);
  // 精确记录实际库和业务表；测试数据仅在场景目录，失败不继续追加写入。
  const counts = await pool.query(
    'SELECT (SELECT COUNT(*) FROM users) AS users, (SELECT COUNT(*) FROM host_devices) AS devices, (SELECT COUNT(*) FROM host_device_users) AS grants, (SELECT COUNT(*) FROM management_audit) AS audit, (SELECT COUNT(*) FROM providers) AS providers',
  );
  console.log('M1 isolated database counts', counts.rows[0]);
}, 120000);

let modelReplies = 0;
const upstream = createServer((req, res) => {
  const chunks: Buffer[] = [];
  req.on('data', (chunk: Buffer) => chunks.push(chunk));
  req.on('end', () => {
    if (req.url?.endsWith('/models')) {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ object: 'list', data: [{ id: 'gpt-6-astra', object: 'model' }] }));
      return;
    }
    const body = JSON.parse(Buffer.concat(chunks).toString()) as {
      messages: { role: string; content: unknown; tool_call_id?: string }[];
      tools?: { function: { name: string } }[];
    };
    const lastUser = body.messages.findLastIndex((message) => message.role === 'user');
    const prompt = JSON.stringify(body.messages[lastUser]?.content ?? '');
    const completed = body.messages.slice(lastUser + 1).some((message) => message.role === 'tool');
    let name = '',
      args: object = {};
    if (
      !completed &&
      prompt.includes('M1_DELEGATE') &&
      body.tools?.some((tool) => tool.function.name === 'delegate_task')
    ) {
      name = 'delegate_task';
      args = { title: 'M1 image child', task: 'M1_READ_IMAGE' };
    } else if (
      !completed &&
      prompt.includes('M1_READ_IMAGE') &&
      body.tools?.some((tool) => tool.function.name === 'read_image')
    ) {
      name = 'read_image';
      args = { path: join(root, 'target-workspace/fixture.png') };
    }
    const delta = name
      ? {
          role: 'assistant',
          tool_calls: [
            { index: 0, id: `m1-${name}`, type: 'function', function: { name, arguments: JSON.stringify(args) } },
          ],
        }
      : { role: 'assistant', content: 'M1 complete' };
    const chunk = {
      id: `m1-response-${++modelReplies}`,
      object: 'chat.completion.chunk',
      model: 'gpt-6-astra',
      choices: [{ index: 0, delta, finish_reason: null }],
    };
    const end = {
      ...chunk,
      choices: [{ index: 0, delta: {}, finish_reason: name ? 'tool_calls' : 'stop' }],
      usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 },
    };
    res.setHeader('content-type', 'text/event-stream');
    res.end(`data: ${JSON.stringify(chunk)}\n\ndata: ${JSON.stringify(end)}\n\ndata: [DONE]\n\n`);
  });
});

async function configureModel() {
  await new Promise<void>((done) => upstream.listen(0, '127.0.0.1', done));
  const address = upstream.address();
  if (!address || typeof address === 'string') throw new Error('missing fixture port');
  const provider = await app!.inject({
    method: 'POST',
    url: '/api/model-providers',
    headers: headers(admin),
    payload: {
      connection: {
        display_name: 'M1 loopback fixture',
        provider_type: 'openai',
        endpoint: `http://127.0.0.1:${address.port}/v1`,
        protocol_preference: 'chat_completions',
        models_path: '',
        discovery_format: 'openai',
        server_history_enabled: false,
        server_history_support_override: null,
      },
      credential: { mode: 'replace', value: 'm1-fixture' },
    },
  });
  expect(provider.statusCode, provider.body).toBe(201);
  const providerId = provider.json<{ provider_instance_id: string }>().provider_instance_id;
  const refreshed = await app!.inject({
    method: 'POST',
    url: `/api/model-providers/${providerId}/refresh`,
    headers: headers(admin),
    payload: {},
  });
  expect(refreshed.statusCode, refreshed.body).toBe(200);
  const selected = await app!.inject({
    method: 'PUT',
    url: '/api/model-settings',
    headers: headers(admin),
    payload: { default_model: { provider_instance_id: providerId, model_id: 'gpt-6-astra' } },
  });
  expect(selected.statusCode, selected.body).toBe(200);
}
