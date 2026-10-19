import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
import { afterAll, expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { emptyParameters } from '../../dist/models/parameters.js';
import type { ProviderConnection } from '../../dist/contracts/models.js';

// 调用方须新建场景及空库；不接受用户库或复用有数据的测试库。
const root = resolve(process.env.CENTER_F05_SCENARIO ?? '');
if (!process.env.CENTER_F05_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f05-m0-')))
  throw new Error('必须使用全新 F05 M0 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string; postgres_port: number; center_port: number; upstream_port: number;
};
if (scenario.root !== root) throw new Error('场景目录不符');
const url = `postgres://f05_app@127.0.0.1:${scenario.postgres_port}/f05_m0`;
const pool = createPool({ url }), source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
const paths: string[] = [];
const upstream = createServer((request, response) => {
  paths.push(`${request.method} ${request.url}`);
  response.writeHead(200, { 'content-type': 'application/json' });
  response.end(JSON.stringify({ object: 'list', data: [{ id: 'gpt-6-astra', object: 'model' }] }));
});
afterAll(async () => {
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
  upstream.closeAllConnections();
  if (upstream.listening) await new Promise<void>((done) => upstream.close(() => done()));
});

it('正式接口保存和发现 OpenCode，保留企业身份并在后台发送前拒绝推理测试', async () => {
  const initial = await pool.query('SELECT host(inet_server_addr()) AS host, inet_server_port() AS port, current_database() AS db');
  expect(initial.rows[0]).toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'f05_m0' });
  expect((await pool.query("SELECT count(*)::integer AS n FROM information_schema.tables WHERE table_schema='public'")).rows[0].n).toBe(0);
  await upgradeDatabase(pool);
  await source.initialize();
  await new Promise<void>((done) => upstream.listen(scenario.upstream_port, '127.0.0.1', done));
  app = await createHttpApp({ source, origin: `http://127.0.0.1:${scenario.center_port}` });
  const login = await app.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'admin', password: '123456' } });
  expect(login.statusCode).toBe(200);
  const token = login.json<{ token: string }>().token;
  const request = async (method: 'GET' | 'POST' | 'PUT', path: string, payload?: object, status = 200) => {
    const response = await app!.inject({ method, url: `/api/${path}`, headers: { authorization: `Bearer ${token}` }, ...(payload ? { payload } : {}) });
    expect(response.statusCode, response.body).toBe(status);
    expect(response.body).not.toContain('f05-fixture-secret');
    return response.json();
  };
  for (const preference of ['chat_completions', 'responses'] as const) {
    const connection: ProviderConnection = {
      display_name: `OpenCode ${preference}`, provider_type: 'opencode',
      endpoint: `http://127.0.0.1:${scenario.upstream_port}/inference/go/openai/v1`,
      models_path: '/inference/go/v1/models', protocol_preference: preference,
      discovery_format: 'openai', responses_transport: 'http',
      server_history_enabled: false, server_history_support_override: null,
    };
    const created = await request('POST', 'model-providers', { connection, credential: { mode: 'replace', value: 'f05-fixture-secret' } }, 201);
    const id: string = created.provider_instance_id;
    expect((await request('GET', `model-providers/${id}`)).connection).toEqual(connection);
    const catalog = await request('POST', `model-providers/${id}/refresh`, {});
    expect(catalog.models[0].model_id).toBe('gpt-6-astra');
    // 同名 OpenAI 模板不能跨 Provider 填充，目录仍保留未知能力。
    expect(catalog.models[0].metadata.streaming).toBe('unknown');
    expect(catalog.models[0].configuration.uses_template).toBe(false);
    const selection = { provider_instance_id: id, model_id: 'gpt-6-astra' };
    const parameters = { ...emptyParameters(), context_window_tokens: { state: 'known', value: 8192 }, max_output_tokens: { state: 'known', value: 1024 }, streaming: 'supported' };
    await request('PUT', 'model-configuration', { ...selection, origin: 'online', parameters });
    expect((await request('PUT', 'model-settings', { default_model: selection })).state).toBe('ready');
    const managed = await request('GET', 'runtime/model-configuration');
    expect(managed.provider_type).toBe('opencode');
    expect(managed.protocol).toBe(preference === 'responses' ? 'open_ai_responses' : 'open_ai_chat_completions');
    expect(managed.endpoint).toBe(`http://127.0.0.1:${scenario.center_port}/api/llm/providers/${id}/v1`);
    expect(managed.server_history_enabled).toBe(false);
    const rejected = await request('POST', 'model-configuration/test', selection, 400);
    expect(rejected.error.code).toBe('MODEL_TEST_REQUIRES_CLIENT');
    const edited = { ...connection, display_name: `edited ${preference}`, models_path: '/inference/v1/models' };
    await request('PUT', `model-providers/${id}`, { connection: edited, credential: { mode: 'unchanged' } });
    expect((await request('GET', `model-providers/${id}/catalog`)).connection_changed).toBe(true);
    await request('POST', `model-providers/${id}/refresh`, {});
    expect((await request('GET', `model-providers/${id}/catalog`)).connection_changed).toBe(false);
    await request('PUT', `model-providers/${id}`, { connection: { ...edited, protocol_preference: 'auto' }, credential: { mode: 'unchanged' } }, 400);
    await request('PUT', `model-providers/${id}`, { connection: { ...edited, provider_type: 'unknown' }, credential: { mode: 'unchanged' } }, 400);
  }
  expect(paths).toEqual(['GET /inference/go/v1/models', 'GET /inference/v1/models', 'GET /inference/go/v1/models', 'GET /inference/v1/models']);
  const counts = (await source.query('SELECT (SELECT count(*)::integer FROM providers) AS providers, (SELECT count(*)::integer FROM model_fixed_configs) AS fixed, (SELECT count(*)::integer FROM model_settings) AS settings, (SELECT count(*)::integer FROM llm_calls) AS calls'))[0];
  expect(counts).toEqual({ providers: 2, fixed: 2, settings: 1, calls: 0 });
  const saved = await source.query('SELECT connection, api_key FROM providers');
  expect(saved.every((row: { connection: ProviderConnection; api_key: string }) => row.connection.provider_type === 'opencode' && row.api_key === 'f05-fixture-secret')).toBe(true);
});
