import { createServer } from 'node:http';
import { readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { WebSocketServer } from 'ws';
import { expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { emptyParameters } from '../../dist/models/parameters.js';

/** 仅显式新建的隔离库；不读取 CENTER_DATABASE_URL 或默认运行环境。 */
it.skipIf(!process.env.CENTER_F01_SCENARIO)('真实数据库准入与 HTTP/WS 压缩、计量及调用账本贯通', async () => {
  const root = resolve(process.env.CENTER_F01_SCENARIO!);
  if (!root.startsWith(resolve('../../.runtime-test/v030-f01-center-'))) throw new Error('隔离路径不符');
  const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
    root: string; postgres_host: string; postgres_port: number; database: string; role: string;
  };
  if (scenario.root !== root || scenario.postgres_host !== '127.0.0.1' || scenario.database !== 'ez_center_f01' || scenario.role !== 'center_app') throw new Error('隔离目标不符');
  const url = `postgres://center_app@127.0.0.1:${scenario.postgres_port}/ez_center_f01`;
  const pool = createPool({ url });
  const source = createDataSource({ url });
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  const http: string[] = [];
  const wire: { connection: number; body: Record<string, unknown> }[] = [];
  const native = { type: 'compaction', id: 'cmp_item', encrypted_content: 'fixture-opaque', extra: { retained: true } };
  const usage = { input_tokens: 100, output_tokens: 10, total_tokens: 110 };
  const upstream = createServer(async (request, response) => {
    http.push(request.url!);
    const parts: Buffer[] = [];
    for await (const part of request) parts.push(part);
    expect(request.headers['x-ez-center-history-binding']).toBeUndefined();
    const body = JSON.parse(Buffer.concat(parts).toString());
    response.writeHead(200, { 'content-type': 'application/json' });
    if (request.url?.endsWith('/input_tokens')) response.end('{"input_tokens":42}');
    else response.end(JSON.stringify({ id: `cmp_http_${http.length}`, object: 'response.compaction', created_at: 42, output: [...body.input.filter((item: { role?: string }) => item.role === 'user'), native], usage }));
  });
  const ws = new WebSocketServer({ server: upstream });
  const fixture = (await readFile('../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse', 'utf8')).split('\n').filter(line => line.startsWith('data: {')).map(line => line.slice(6));
  let connections = 0;
  ws.on('connection', socket => {
    const connection = ++connections;
    socket.on('message', raw => {
      const body = JSON.parse(raw.toString()) as Record<string, unknown>;
      wire.push({ connection, body });
      const input = body.input as { type?: string }[];
      if (input.at(-1)?.type === 'compaction_trigger') socket.send(JSON.stringify({ type: 'response.completed', response: { id: `cmp_ws_${wire.length}`, created_at: 42, status: 'completed', output: [native], usage } }));
      else for (const event of fixture) socket.send(event.replaceAll('resp_text_1', `resp_up_${wire.length}`));
    });
  });
  try {
    const actual = (await pool.query('SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role')).rows[0];
    expect(actual).toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'ez_center_f01', role: 'center_app' });
    expect((await pool.query("SELECT COUNT(*)::text AS n FROM pg_tables WHERE schemaname='public'")).rows[0].n).toBe('0');
    await upgradeDatabase(pool);
    await source.initialize();
    await new Promise<void>(done => upstream.listen(0, '127.0.0.1', done));
    const address = upstream.address();
    if (!address || typeof address === 'string') throw new Error('missing upstream port');
    const templatesFile = join(root, 'templates.json');
    await writeFile(templatesFile, '[]');
    app = await createHttpApp({ source, origin: 'http://127.0.0.1', templatesFile, pluginRoot: join(root, 'plugins'), snapshotRoot: join(root, 'snapshots') });
    await app.listen(0, '127.0.0.1');
    const origin = await app.getUrl();
    const login = async (username: string, password: string) => {
      const response = await fetch(`${origin}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username, password }) });
      expect(response.status).toBe(200);
      return response.json();
    };
    const administrator = await login('admin', '123456');
    const api = async (path: string, method = 'GET', body?: object, status = 200, token = administrator.token) => {
      const response = await fetch(`${origin}/api/${path}`, { method, headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' }, ...(body ? { body: JSON.stringify(body) } : {}) });
      const text = await response.text();
      expect(response.status, `${path}: ${text}`).toBe(status);
      return text ? JSON.parse(text) : undefined;
    };
    await api('model-settings', 'PUT', { public_origin: origin });
    const connection = { display_name: 'Native fixture', provider_type: 'openai', endpoint: `http://127.0.0.1:${address.port}/v1`, protocol_preference: 'responses', models_path: '', discovery_format: 'openai', server_history_enabled: false, server_history_support_override: 'supported', responses_transport: 'http' };
    const provider = await api('model-providers', 'POST', { connection, credential: { mode: 'clear' } }, 201);
    const selected = { provider_instance_id: provider.provider_instance_id, model_id: 'fixture' };
    const parameters = { ...emptyParameters(), context_window_tokens: { state: 'known', value: 128000 }, max_output_tokens: { state: 'known', value: 1024 }, streaming: 'supported', tool_calls: 'supported', server_compaction_enabled: true, server_compaction_support: 'supported', input_token_count_support: 'supported' };
    await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters });
    await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters: { ...parameters, server_auto_compaction_enabled: true } }, 400);
    await api('model-settings', 'PUT', { default_model: selected });
    for (const username of ['alice', 'bob']) await api('users', 'POST', { username, display_name: username, role: 'user', enabled: true, password: 'fixture-password1' }, 201);
    const alice = await login('alice', 'fixture-password1'), bob = await login('bob', 'fixture-password1');
    const config = () => api('runtime/model-configuration', 'GET', undefined, 200, alice.token);
    let binding = (await config()).server_history_binding;
    expect(binding).toMatch(/^[a-f0-9]{64}$/);
    const request = async (suffix: string, body: object, status = 200, bound: string | undefined = binding, key = alice.llm_key) => {
      const response = await fetch(`${origin}/api/llm/providers/${provider.provider_instance_id}/v1/${suffix}`, { method: 'POST', headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json', ...(bound === undefined ? {} : { 'x-ez-center-history-binding': bound }) }, body: JSON.stringify(body) });
      const text = await response.text();
      expect(response.status, text).toBe(status);
      return { text, call: response.headers.get('x-ez-call-id') };
    };
    const user = { role: 'user', content: 'retain this original instruction' };
    const compactBody = { model: 'fixture', input: [user], instructions: 'fixture instructions' };
    const first = await request('responses/compact', compactBody);
    expect(JSON.parse(first.text).output).toEqual([user, native]);
    await request('responses/compact', compactBody, 409, 'wrong');
    await request('responses/compact', compactBody, 409, binding, bob.llm_key);
    await request('responses/compact', { ...compactBody, store: false }, 400);
    await request('responses', { model: 'fixture', stream: true, store: true, input: [user] }, 409);
    await api(`model-providers/${provider.provider_instance_id}`, 'PUT', { connection: { ...connection, responses_transport: 'websocket' }, credential: { mode: 'unchanged' } });
    const old = binding;
    binding = (await config()).server_history_binding;
    expect(binding).not.toBe(old);
    await request('responses/compact', compactBody, 409, old);
    const second = await request('responses/compact', compactBody);
    expect(JSON.parse(second.text).output).toEqual([user, native]);
    await request('responses/compact', { ...compactBody, input: JSON.parse(second.text).output });
    const count = await request('responses/input_tokens', { model: 'fixture', input: JSON.parse(second.text).output });
    expect(JSON.parse(count.text)).toEqual({ input_tokens: 42 });
    await request('responses', { model: 'fixture', stream: true, store: false, input: JSON.parse(second.text).output });
    expect(http).toEqual(['/v1/responses/compact', '/v1/responses/input_tokens']);
    expect(wire.map(item => item.connection)).toEqual([1, 2, 3]);
    expect(wire.every(item => !Object.hasOwn(item.body, 'previous_response_id') && item.body.store === false)).toBe(true);
    const rows = (await pool.query('SELECT kind,usage_state,input_tokens,total_tokens FROM llm_calls ORDER BY started_at,id')).rows;
    expect(rows.filter(row => row.kind === 'context_compaction' && row.usage_state === 'available')).toHaveLength(3);
    expect(rows.filter(row => row.kind === 'input_tokens')).toEqual([{ kind: 'input_tokens', usage_state: 'unavailable', input_tokens: null, total_tokens: null }]);
    const counted = await api('llm-usage');
    expect(counted.summary.calls).toBe(String(rows.filter(row => ['proxy', 'context_compaction'].includes(row.kind)).length));
    expect(counted.summary.total_tokens).toBe(String(rows.filter(row => ['proxy', 'context_compaction'].includes(row.kind)).reduce((sum, row) => sum + Number(row.total_tokens ?? 0), 0)));
    const counts: Record<string, string> = {};
    for (const { tablename } of (await pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")).rows) {
      if (!/^[a-z_]+$/.test(tablename)) throw new Error('unexpected table');
      counts[tablename] = (await pool.query(`SELECT COUNT(*)::text AS n FROM public.${tablename}`)).rows[0].n;
    }
    await writeFile(join(root, 'verified-counts.json'), JSON.stringify(counts));
    expect(counts.providers).toBe('1');
    expect(counts.model_fixed_configs).toBe('1');
    expect(counts.users).toBe('3');
    expect(counts.pgmigrations).toBe('10');
  } finally {
    await app?.close();
    for (const socket of ws.clients) socket.terminate();
    await new Promise<void>(done => ws.close(() => done()));
    upstream.closeAllConnections();
    if (upstream.listening) await new Promise<void>(done => upstream.close(() => done()));
    if (source.isInitialized) await source.destroy();
    await pool.end();
  }
}, 30000);
