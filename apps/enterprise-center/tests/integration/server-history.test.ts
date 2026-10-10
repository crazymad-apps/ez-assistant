import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createServer } from 'node:http';
import { spawn, execFile, type ChildProcess } from 'node:child_process';
import { promisify } from 'node:util';
import { randomUUID } from 'node:crypto';
import { runner } from 'node-pg-migrate';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { readDatabaseVersion } from '../../dist/database/admission.js';
import { createHttpApp } from '../../dist/app.js';
import { ModelsService } from '../../dist/models/service.js';
import { models } from '../../dist/models/repository.js';
import { emptyParameters } from '../../dist/models/parameters.js';

// 只有本轮显式新建并核对的独立 cluster/database 可用；不接受普通 CENTER_DATABASE_URL。
const scenarioPath = process.env.CENTER_F06_SCENARIO;
if (!scenarioPath) throw new Error('须显式提供已核对的新建 F06 M2 场景');
const root = resolve(scenarioPath);
if (!root.startsWith(resolve('../../.runtime-test/f06-m2-'))) throw new Error('隔离路径不符');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string; postgres_host: string; postgres_port: number; database: string; restore_database: string;
  role: string; pg_bin: string; center_port: number; host_port: number;
};
if (scenario.root !== root || scenario.postgres_host !== '127.0.0.1' || scenario.database !== 'ez_center_f06_m2'
  || scenario.restore_database !== 'ez_center_f06_m2_restore' || scenario.role !== 'center_app') throw new Error('隔离目标不符');
const url = `postgres://center_app@127.0.0.1:${scenario.postgres_port}/${scenario.database}`;
const pool = createPool({ url });
const source = createDataSource({ url });
const runFile = promisify(execFile);
const origin = `http://127.0.0.1:${scenario.center_port}`;
const templatesFile = join(root, 'model-templates.json');
const originalTemplates = await readFile('../../packages/assistant-protocol/resources/model-templates.json', 'utf8');
await writeFile(templatesFile, originalTemplates);
const parameters = { ...emptyParameters(), context_window_tokens: { state: 'known' as const, value: 128000 },
  max_output_tokens: { state: 'known' as const, value: 1024 }, streaming: 'supported' as const,
  image_input: 'unsupported' as const, tool_calls: 'supported' as const, reasoning: 'unsupported' as const,
  tool_choice: { auto: 'supported' as const, none: 'supported' as const, required: 'supported' as const, named: 'supported' as const },
  reasoning_mode: 'unsupported' as const, tool_image_projection: 'unsupported' as const };
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
let host: ChildProcess | undefined;
const wire: { raw: string; body: Record<string, unknown>; binding?: string; authorization?: string }[] = [];
const textSse = await readFile('../../crates/agent-openai-compatible/fixtures/responses/openai/text.sse', 'utf8');
const upstream = createServer(async (request, response) => {
  if (request.method === 'GET') {
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ object: 'list', data: ['model-a', 'model-b', 'gpt-6-astra'].map(id => ({ object: 'model', id })) }));
    return;
  }
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.from(chunk));
  const raw = Buffer.concat(chunks).toString();
  const body = JSON.parse(raw) as Record<string, unknown>;
  wire.push({ raw, body, binding: request.headers['x-ez-center-history-binding'] as string | undefined, authorization: request.headers.authorization });
  const index = wire.length;
  if (body.stream === false) {
    response.setHeader('content-type', 'application/json');
    response.end(JSON.stringify({ id: `resp_m2_${index}`, object: 'response', model: body.model,
      output: [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'OK' }] }],
      usage: { input_tokens: 11, output_tokens: 4, total_tokens: 15 } }));
    return;
  }
  response.setHeader('content-type', 'text/event-stream');
  response.end(textSse.replaceAll('resp_text_1', `resp_m2_${index}`).replaceAll('msg_text_1', `msg_m2_${index}`)
    .replaceAll('gpt-test', String(body.model)));
});

async function counts(connection: ReturnType<typeof createPool>) {
  const tables = (await connection.query<{ tablename: string }>("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")).rows;
  const result: Record<string, string> = {};
  for (const { tablename } of tables) {
    if (!/^[a-z_]+$/.test(tablename)) throw new Error('unexpected table');
    result[tablename] = (await connection.query<{ n: string }>(`SELECT COUNT(*) AS n FROM public.${tablename}`)).rows[0]!.n;
  }
  return result;
}

afterAll(async () => {
  if (host?.exitCode === null) {
    host.kill('SIGTERM');
    await new Promise<void>(done => host!.once('exit', () => done()));
  }
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
  upstream.closeAllConnections();
  if (upstream.listening) await new Promise<void>(done => upstream.close(() => done()));
  vi.restoreAllMocks();
});

it('005 保留真实旧表内容，独立备份可恢复，旧 Center 拒绝新账本', async () => {
  const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role")).rows[0];
  expect(actual).toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: scenario.database, role: 'center_app' });
  expect(await counts(pool)).toEqual({});
  const files = upgradeFiles();
  const migrationClient = await pool.connect();
  try {
    await runner({ dbClient: migrationClient, dir: files.directory, direction: 'up', count: 4,
      migrationsTable: 'pgmigrations', singleTransaction: true,
      logger: { info() {}, warn() {}, error() {} } });
  } finally { migrationClient.release(true); }
  const legacy = { display_name: 'Legacy', provider_type: 'openai', endpoint: 'https://example.invalid/v1',
    protocol_preference: 'auto', models_path: '', discovery_format: 'openai' };
  const id = '00000000-0000-0000-0000-000000000001';
  const seed = await pool.connect();
  await seed.query('BEGIN');
  try {
    await seed.query('INSERT INTO providers(provider_instance_id,connection,api_key) VALUES($1,$2,$3)', [id, legacy, 'legacy-artificial-key']);
    await seed.query('INSERT INTO model_fixed_configs(provider_instance_id,model_id,origin,parameters) VALUES($1,$2,$3,$4)', [id, 'legacy-model', 'manual', parameters]);
    await seed.query('UPDATE model_settings SET provider_instance_id=$1,model_id=$2', [id, 'legacy-model']);
    await seed.query('COMMIT');
  } catch (error) { await seed.query('ROLLBACK'); throw error; } finally { seed.release(); }
  const before = await counts(pool);
  const pgArgs = ['-h', '127.0.0.1', '-p', String(scenario.postgres_port), '-U', 'center_app'];
  const backup = join(root, 'before-005.dump');
  await runFile(join(scenario.pg_bin, 'pg_dump'), [...pgArgs, '-Fc', '-f', backup, scenario.database]);
  expect((await readFile(backup)).length).toBeGreaterThan(0);
  await runFile(join(scenario.pg_bin, 'pg_restore'), [...pgArgs, '--no-owner', '--no-privileges', '-d', scenario.restore_database, backup]);
  const restored = createPool({ url: url.replace(scenario.database, scenario.restore_database) });
  try {
    expect(await counts(restored)).toEqual(before);
    expect((await restored.query('SELECT connection,api_key FROM providers')).rows).toEqual([{ connection: legacy, api_key: 'legacy-artificial-key' }]);
  } finally { await restored.end(); }
  expect((await upgradeDatabase(pool)).applied).toEqual(files.names.slice(4));
  const after = await counts(pool);
  expect({ ...after, pgmigrations: before.pgmigrations }).toEqual({ ...before, feedback: '0' });
  expect((await pool.query('SELECT connection,api_key,server_history_key FROM providers')).rows).toEqual([
    { connection: { ...legacy, server_history_enabled: false, server_history_support_override: null }, api_key: 'legacy-artificial-key', server_history_key: null },
  ]);
  expect((await pool.query('SELECT provider_instance_id,model_id FROM model_settings')).rows).toEqual([{ provider_instance_id: id, model_id: 'legacy-model' }]);
  const client = await pool.connect();
  try { await expect(readDatabaseVersion(client, files.names.slice(0, 4))).rejects.toMatchObject({ code: 'SCHEMA_MISMATCH' }); }
  finally { client.release(); }
  await source.initialize();
}, 30000);

it('配置到代理与正式 Host 闭环：主体/策略/ABA 拒绝旧绑定，重登录与重启恢复续接', async () => {
  await new Promise<void>(done => upstream.listen(0, '127.0.0.1', done));
  const address = upstream.address();
  if (!address || typeof address === 'string') throw new Error('missing upstream port');
  const endpoint = `http://127.0.0.1:${address.port}/v1`;
  const openCenter = async () => {
    app = await createHttpApp({ source, origin, templatesFile, pluginRoot: join(root, 'plugins'), snapshotRoot: join(root, 'snapshots') },
      resolve('../enterprise-admin/dist/admin'));
    await app.listen(scenario.center_port, '127.0.0.1');
  };
  await openCenter();
  const login = async (username: string, password = 'fixture-password1') => {
    const response = await fetch(`${origin}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username, password }) });
    expect(response.status).toBe(200);
    return response.json();
  };
  let administrator = await login('admin', '123456');
  async function api(path: string, method = 'GET', payload?: object, status = 200, token = administrator.token) {
    const response = await fetch(`${origin}/api/${path}`, { method,
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      ...(payload === undefined ? {} : { body: JSON.stringify(payload) }) });
    const text = await response.text();
    expect(response.status, `${path}: ${text}`).toBe(status);
    expect(text).not.toContain('upstream-artificial-key');
    return text ? JSON.parse(text) : undefined;
  }
  const info = await api('info');
  expect([info.protocol_version, info.min_protocol_version]).toEqual([2, 2]);
  for (const username of ['alice', 'bob']) await api('users', 'POST', { username, display_name: username, role: 'user', enabled: true, password: 'fixture-password1' }, 201);
  const connection = { display_name: 'F06 M2', provider_type: 'openai', endpoint, protocol_preference: 'responses',
    models_path: '', discovery_format: 'openai', server_history_enabled: true, server_history_support_override: 'supported' };
  await api('model-providers', 'POST', { connection: { ...connection, server_history_support_override: null }, credential: { mode: 'clear' } }, 400);
  const provider = await api('model-providers', 'POST', { connection, credential: { mode: 'replace', value: 'upstream-artificial-key' } }, 201);
  expect(provider.server_history_key).toBeUndefined();
  const id: string = provider.provider_instance_id;
  const selected = { provider_instance_id: id, model_id: 'model-a' };
  for (const model_id of ['model-a', 'model-b']) await api('model-configuration', 'PUT', { ...selected, model_id, origin: 'manual', parameters });
  await api('model-settings', 'PUT', { default_model: selected });
  const saveConnection = (patch: object, credential: object = { mode: 'unchanged' }) => api(`model-providers/${id}`, 'PUT', { connection: { ...connection, ...patch }, credential });
  let alice = await login('alice');
  const bob = await login('bob');
  const config = (token = alice.token) => api('runtime/model-configuration', 'GET', undefined, 200, token);
  let current = await config();
  const firstBinding: string = current.server_history_binding;
  expect(firstBinding).toMatch(/^[a-f0-9]{64}$/);
  expect((await config(bob.token)).server_history_binding).not.toBe(firstBinding);
  alice = await login('alice');
  expect((await config()).server_history_binding).toBe(firstBinding);
  expect((await api('model-configuration/test', 'POST', selected)).connected).toBe(true);
  expect(wire.at(-1)!.body.store).toBe(false);
  expect(wire.at(-1)!.body.previous_response_id).toBeUndefined();
  expect(wire.at(-1)!.binding).toBeUndefined();
  await api(`model-providers/${id}`, 'PUT', { connection: { display_name: 'Old page', provider_type: 'openai', endpoint, protocol_preference: 'responses', models_path: '', discovery_format: 'openai' }, credential: { mode: 'unchanged' } }, 400);
  expect((await config()).server_history_binding).toBe(firstBinding);
  const raw = '{"model":"model-a", "store":true,"stream":true, "n":9007199254740993,"input":"Hello"}';
  const proxy = async (payload = raw, binding: string | undefined = firstBinding, key = alice.llm_key, status = 200) => {
    const response = await fetch(`${origin}/api/llm/providers/${id}/v1/responses`, { method: 'POST',
      headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json', ...(binding === undefined ? {} : { 'x-ez-center-history-binding': binding }) }, body: payload });
    const text = await response.text();
    expect(response.status, text).toBe(status);
  };
  await proxy();
  expect(wire.at(-1)!.raw).toBe(raw);
  expect(wire.at(-1)!.binding).toBeUndefined();
  expect(wire.at(-1)!.authorization).toBe('Bearer upstream-artificial-key');
  const sent = wire.length;
  await proxy(raw, firstBinding, bob.llm_key, 409);
  await proxy(raw, '', alice.llm_key, 409);
  await proxy('{"model":"model-a","store":false,"previous_response_id":"resp_other"}', firstBinding, alice.llm_key, 400);
  await proxy('{"model":"model-a","input":"Hello"}', firstBinding, alice.llm_key, 400);
  expect(wire.length).toBe(sent);
  await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters });
  await saveConnection({ display_name: 'Renamed' });
  expect((await config()).server_history_binding).toBe(firstBinding);
  const changed = async (action: () => Promise<unknown>) => {
    const before = (await config()).server_history_binding;
    await action();
    const after = (await config()).server_history_binding;
    expect(after).not.toBe(before);
    await proxy(raw, before, alice.llm_key, 409);
    current = await config();
  };
  await changed(async () => { await saveConnection({ server_history_enabled: false }); await proxy(raw, firstBinding, alice.llm_key, 409); await saveConnection({}); });
  await changed(async () => { await saveConnection({ endpoint: endpoint + '/other' }); await saveConnection({}); });
  await changed(async () => { await saveConnection({}, { mode: 'replace', value: 'new-artificial-key' }); await saveConnection({}, { mode: 'replace', value: 'upstream-artificial-key' }); });
  await changed(async () => { await api('model-settings', 'PUT', { default_model: { ...selected, model_id: 'model-b' } }); await api('model-settings', 'PUT', { default_model: selected }); });
  await changed(async () => { await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters: { ...parameters, max_output_tokens: { state: 'known', value: 512 } } }); await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters }); });
  await changed(async () => { await api('model-configuration/reset', 'POST', selected, 204); await api('model-configuration', 'PUT', { ...selected, origin: 'manual', parameters }); });

  await api(`model-providers/${id}/refresh`, 'POST', {});
  await api('model-settings', 'PUT', { default_model: { ...selected, model_id: 'gpt-6-astra' } });
  const templateBinding = (await config()).server_history_binding;
  await api('model-templates/reload', 'POST', {});
  expect((await config()).server_history_binding).toBe(templateBinding);
  const altered = JSON.parse(originalTemplates);
  altered.find((entry: { provider_type: string; model_id: string }) => entry.provider_type === 'openai' && entry.model_id === 'gpt-6-astra').parameters.max_output_tokens.value = 1024;
  await writeFile(templatesFile, JSON.stringify(altered));
  const templateOwner = app!.get(ModelsService).templates;
  const loaded = templateOwner.status().loaded_at_ms;
  const failure = vi.spyOn(models, 'renewHistory').mockRejectedValueOnce(new Error('artificial persistence failure'));
  await api('model-templates/reload', 'POST', {}, 503);
  expect(templateOwner.status().loaded_at_ms).toBe(loaded);
  expect((await config()).server_history_binding).toBe(templateBinding);
  failure.mockRestore();
  await api('model-templates/reload', 'POST', {});
  await writeFile(templatesFile, originalTemplates);
  await api('model-templates/reload', 'POST', {});
  expect((await config()).server_history_binding).not.toBe(templateBinding);
  await api('model-settings', 'PUT', { default_model: selected });
  const restartBinding = (await config()).server_history_binding;
  await app!.close();
  await openCenter();
  administrator = await login('admin', '123456');
  alice = await login('alice');
  expect((await config()).server_history_binding).toBe(restartBinding);

  // 使用正式 Host 二进制及本轮全新用户 SQLite，启动期间不重建或替换该二进制。
  const home = join(root, 'host');
  await mkdir(home);
  await writeFile(join(home, 'host.toml'), `version='0.27.0'\nmode='enterprise'\n[enterprise]\ncenter_url='${origin}'\n[host_access]\nport=${scenario.host_port}\nremote_enabled=false\n`);
  let discovery: { address: string; access_token: string };
  const startHost = async () => {
    host = spawn(resolve('../../target/debug/ez-assistant-runtime'), ['serve', '--runtime-home', home], { stdio: ['ignore', 'ignore', 'pipe'] });
    let stderr = '';
    host.stderr!.on('data', chunk => { stderr += String(chunk); });
    await expect.poll(async () => {
      if (host!.exitCode !== null) throw new Error(stderr);
      try { discovery = JSON.parse(await readFile(join(home, 'run/runtime.json'), 'utf8')); return (await fetch(`${discovery.address}/health`, { headers: headers(discovery.access_token) })).ok; } catch { return false; }
    }, { timeout: 15000 }).toBe(true);
  };
  const headers = (token: string) => ({ authorization: `Bearer ${token}`, 'content-type': 'application/json',
    'x-ez-client-version': '0.29.0', 'x-ez-min-compatible-version': '0.29.0' });
  const hostLogin = async () => {
    const { authorization: _authorization, ...loginHeaders } = headers('');
    const response = await fetch(`${discovery.address}/auth/login`, { method: 'POST', headers: loginHeaders,
      body: JSON.stringify({ method: 'enterprise', username: 'alice', password: 'fixture-password1', native: true }) });
    expect(response.status).toBe(200);
    return (await response.json()).token as string;
  };
  const command = async (token: string, type: string, payload: object) => {
    const response = await fetch(`${discovery.address}/commands`, { method: 'POST', headers: headers(token),
      body: JSON.stringify({ request_id: randomUUID(), command: { scope: 'runtime', payload: { type, payload } } }) });
    const result = await response.json();
    expect(response.ok, JSON.stringify(result)).toBe(true);
    return result.result.payload.payload;
  };
  const stopHost = async () => {
    await command(discovery.access_token, 'shutdown_runtime', {});
    await expect.poll(() => host!.exitCode, { timeout: 15000 }).not.toBeNull();
  };
  await startHost();
  let token = await hostLogin();
  await command(token, 'refresh_model_source', {});
  const settings = await command(token, 'get_model_settings', {});
  expect(settings.management.read_only).toBe(true);
  expect((await command(token, 'list_providers', {}))[0].connection.server_history_enabled).toBe(true);
  const validation = await command(token, 'validate_model_connection', {
    selection: { provider_instance_id: 'managed', model_id: 'model-a' },
  });
  expect(validation.outcome.status).toBe('succeeded');
  expect(wire.at(-1)!.body.store).toBe(false);
  expect(wire.at(-1)!.body.previous_response_id).toBeUndefined();
  const created = await command(token, 'create_session', { title: 'F06 M2', workspace_id: null, model_selection: null });
  const session_id: string = created.session.session_id;
  const run = async (message: string, expected = 'completed') => {
    const submitted = await command(token, 'submit_input', { session_id, message, variant: 'build' });
    await expect.poll(async () => (await command(token, 'get_run', { session_id, run_id: submitted.run.run_id })).run.status, { timeout: 15000 }).toBe(expected);
  };
  const calls = () => wire.filter(call => call.body.store === true && JSON.stringify(call.body.input).includes('M2_HOST_FIRST'));
  await run('M2_HOST_FIRST');
  const first = calls().at(-1)!;
  expect(first.body.previous_response_id).toBeUndefined();
  const firstId = `resp_m2_${wire.indexOf(first) + 1}`;
  await run('M2_HOST_SECOND');
  const second = wire.filter(call => call.body.store === true).at(-1)!;
  expect(second.body.previous_response_id).toBe(firstId);
  const secondId = `resp_m2_${wire.indexOf(second) + 1}`;
  token = await hostLogin();
  await command(token, 'refresh_model_source', {});
  await run('M2_HOST_RELOGIN');
  const relogin = wire.filter(call => call.body.store === true).at(-1)!;
  expect(relogin.body.previous_response_id).toBe(secondId);
  const reloginId = `resp_m2_${wire.indexOf(relogin) + 1}`;
  await stopHost();
  await startHost();
  token = await hostLogin();
  await command(token, 'refresh_model_source', {});
  await run('M2_HOST_RESTART');
  expect(wire.filter(call => call.body.store === true).at(-1)!.body.previous_response_id).toBe(reloginId);
  const beforeRejected = wire.filter(call => call.body.store === true).length;
  await saveConnection({}, { mode: 'replace', value: 'changed-upstream-key' });
  await run('M2_HOST_STALE', 'failed');
  expect(wire.filter(call => call.body.store === true).length).toBe(beforeRejected);
  await run('M2_HOST_REBUILT');
  const rebuilt = wire.filter(call => call.body.store === true).at(-1)!;
  expect(rebuilt.body.previous_response_id).toBeUndefined();
  expect(JSON.stringify(rebuilt.body.input)).toContain('M2_HOST_FIRST');
  expect(wire.every(call => call.binding === undefined)).toBe(true);
  await stopHost();
  const tableCounts = await counts(pool);
  expect(Number(tableCounts.providers)).toBe(2);
  expect(Number(tableCounts.model_fixed_configs)).toBe(3);
  expect(Number(tableCounts.users)).toBe(3);
  expect((await pool.query("SELECT COUNT(*)::int AS n FROM llm_calls WHERE outcome='in_progress'")).rows[0].n).toBe(0);
  const callOutcomes = (await pool.query('SELECT outcome,COUNT(*)::integer AS n FROM llm_calls GROUP BY outcome ORDER BY outcome')).rows;
  await writeFile(join(root, 'verified.json'), JSON.stringify({ target: { host: '127.0.0.1', port: scenario.postgres_port, database: scenario.database, role: scenario.role }, counts: tableCounts,
    callOutcomes, backup: join(root, 'before-005.dump'), restore_database: scenario.restore_database, host_home: home, wire_count: wire.length }, null, 2));
}, 120000);
