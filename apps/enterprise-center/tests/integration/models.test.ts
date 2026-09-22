import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createServer } from 'node:http';
import { afterAll, expect, it } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { createDataSource } from '../../dist/database/source.js';
import type { ModelParameters, ProviderConnection } from '../../dist/contracts/models.js';

const url = process.env.CENTER_TEST_DATABASE_URL;
if (!url || process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true')
  throw new Error('先核实独立 C04 数据库并备份恢复验证');
const target = new URL(url);
if (
  target.hostname !== '127.0.0.1' ||
  target.port !== '55434' ||
  target.pathname !== '/ez_center_c04_m0' ||
  target.username !== 'center_app'
)
  throw new Error('仅允许专用 C04 隔离库');
const root = resolve('../../.runtime-test/c04-m0-isolated-20260921');
const directory = mkdtempSync(join(root, 'models-'));
const templatesFile = join(directory, 'model-templates.json');
const templateText = readFileSync('../../packages/assistant-protocol/resources/model-templates.json', 'utf8');
writeFileSync(templatesFile, templateText);
const source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
let body: object = { object: 'list', data: [{ object: 'model', id: 'gpt-6-astra' }] };
let fail = false,
  hold: (() => void) | undefined,
  arrived: (() => void) | undefined;
const upstream = createServer((_req, res) => {
  const respond = () => {
    res.writeHead(fail ? 503 : 200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(body));
  };

  if (arrived) {
    hold = respond;
    arrived();
    arrived = undefined;
  } else respond();
});
afterAll(async () => {
  hold?.();
  await app?.close();
  if (source.isInitialized) await source.destroy();
  upstream.closeAllConnections();
  await new Promise<void>((resolve) => upstream.close(() => resolve()));
});

it('真实数据库与 HTTP 保持目录/固定/模板独立，拒绝迟到覆盖并保留删除后的默认引用', async () => {
  await source.initialize();
  expect((await source.query('SELECT count(*)::integer AS n FROM providers'))[0].n).toBe(0);
  await new Promise<void>((resolve) => upstream.listen(0, '127.0.0.1', resolve));
  const address = upstream.address();
  if (!address || typeof address === 'string') throw new Error('missing port');
  app = await createHttpApp({ source, origin: 'http://127.0.0.1:17320', templatesFile });
  const login = await app.inject({
    method: 'POST',
    url: '/api/auth/login',
    payload: { username: 'admin', password: '123456' },
  });
  expect(login.statusCode).toBe(200);
  const token: string = login.json().token;

  async function request(method: 'GET' | 'POST' | 'PUT' | 'DELETE', path: string, payload?: object, status = 200) {
    const response = await app!.inject({
      method,
      url: `/api/${path}`,
      headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
      ...(payload === undefined ? {} : { payload }),
    });
    expect(response.statusCode, `${method} ${path}: ${response.body}`).toBe(status);
    expect(response.body).not.toContain('isolated-provider-secret');
    return response.statusCode === 204 ? undefined : response.json();
  }

  const connection: ProviderConnection = {
    display_name: '隔离模型服务',
    provider_type: 'openai',
    endpoint: `http://127.0.0.1:${address.port}/v1`,
    protocol_preference: 'auto',
    models_path: '',
    discovery_format: 'openai',
  };
  const created = await request(
    'POST',
    'model-providers',
    { connection, credential: { mode: 'replace', value: 'isolated-provider-secret' } },
    201,
  );
  const id: string = created.provider_instance_id,
    selection = { provider_instance_id: id, model_id: 'gpt-6-astra' };
  expect((await request('GET', 'model-providers'))[0].has_api_key).toBe(true);
  expect((await request('GET', `model-providers/${id}/catalog`)).refreshed_at_ms).toBeNull();
  const refreshed = await request('POST', `model-providers/${id}/refresh`, {});
  expect(refreshed.models[0].metadata.context_window_tokens.state).toBe('unknown');
  expect(refreshed.models[0].configuration.uses_template).toBe(true);
  const detail = await request('POST', 'model-configuration/query', { ...selection, origin: 'online' });
  expect(detail.parameters.tool_image_projection).toBe('native_tool_result');
  const parameters: ModelParameters = detail.parameters;
  parameters.max_output_tokens = { state: 'known', value: 1024 };
  await request('PUT', 'model-configuration', { ...selection, origin: 'online', parameters });
  expect((await request('PUT', 'model-settings', { default_model: selection })).state).toBe('ready');
  const managed = await request('GET', 'runtime/model-configuration');
  expect(managed.parameters.max_output_tokens.value).toBe(1024);
  expect(managed.endpoint).toContain(`/providers/${id}/v1`);
  const stored = (await source.query('SELECT model_catalog FROM providers WHERE provider_instance_id=$1', [id]))[0]
    .model_catalog;
  expect(stored[0].configuration).toBeUndefined();
  expect(stored[0].metadata.context_window_tokens.state).toBe('unknown');
  fail = true;
  await request('POST', `model-providers/${id}/refresh`, {}, 502);
  fail = false;
  expect((await request('GET', `model-providers/${id}/catalog`)).refreshed_at_ms).toBe(refreshed.refreshed_at_ms);
  const reached = new Promise<void>((resolve) => {
    arrived = resolve;
  });
  const late = app!.inject({
    method: 'POST',
    url: `/api/model-providers/${id}/refresh`,
    headers: { authorization: `Bearer ${token}` },
    payload: {},
  });
  const lateResult = Promise.resolve(late);
  await reached;
  await request('PUT', `model-providers/${id}`, {
    connection: { ...connection, models_path: '/new-models' },
    credential: { mode: 'unchanged' },
  });
  hold!();
  hold = undefined;
  expect((await lateResult).statusCode).toBe(409);
  expect((await request('GET', `model-providers/${id}/catalog`)).connection_changed).toBe(true);
  body = { object: 'list', data: [] };
  await request('POST', `model-providers/${id}/refresh`, {});
  expect((await request('GET', `model-providers/${id}/catalog`)).models).toEqual([]);
  expect((await request('GET', `model-providers/${id}/fixed-configs`)).items).toHaveLength(1);
  expect((await request('GET', 'model-settings')).state).toBe('ready');
  // 新 HTTP/模板 owner 只从数据库和静态文件恢复，不复用进程模型目录缓存。
  await app!.close();
  app = await createHttpApp({ source, origin: 'http://127.0.0.1:17320', templatesFile });
  const relogin = await app.inject({
    method: 'POST',
    url: '/api/auth/login',
    payload: { username: 'admin', password: '123456' },
  });
  expect(relogin.statusCode).toBe(200);
  const newToken: string = relogin.json().token;

  const next = async (path: string, method: 'GET' | 'POST' = 'GET', payload?: object, expected = 200) => {
    const response = await app!.inject({
      method,
      url: `/api/${path}`,
      headers: { authorization: `Bearer ${newToken}`, 'content-type': 'application/json' },
      ...(payload === undefined ? {} : { payload }),
    });
    expect(response.statusCode).toBe(expected);
    return expected === 204 ? undefined : response.json();
  };

  expect((await next(`model-providers/${id}/catalog`)).models).toEqual([]);
  expect((await next('model-settings')).state).toBe('ready');
  const status = await next('model-templates');
  expect(status.available).toBe(true);
  writeFileSync(templatesFile, '{invalid');
  await next('model-templates/reload', 'POST', {}, 400);
  expect((await next('model-templates')).loaded_at_ms).toBe(status.loaded_at_ms);
  const updated = JSON.parse(templateText);
  updated.find(
    (entry: { provider_type: string; model_id: string }) =>
      entry.provider_type === 'openai' && entry.model_id === selection.model_id,
  ).parameters.max_output_tokens.value = 2048;
  writeFileSync(templatesFile, JSON.stringify(updated));
  await next('model-templates/reload', 'POST', {});
  expect((await next('runtime/model-configuration')).parameters.max_output_tokens.value).toBe(1024);
  await next('model-configuration/reset', 'POST', selection, 204);
  expect((await next('model-settings')).reason).toBe('model_absent');
  const deletion = await app.inject({
    method: 'DELETE',
    url: `/api/model-providers/${id}`,
    headers: { authorization: `Bearer ${newToken}` },
  });
  expect(deletion.statusCode).toBe(200);
  expect(deletion.json().default_model).toBe(true);
  expect((await next('model-settings')).reason).toBe('provider_deleted');
  const final = (await source.query('SELECT provider_instance_id,model_id FROM model_settings'))[0];
  expect(final).toEqual(selection);
  expect((await source.query('SELECT count(*)::integer AS n FROM providers'))[0].n).toBe(0);
  expect((await source.query('SELECT count(*)::integer AS n FROM model_fixed_configs'))[0].n).toBe(0);
  expect((await app.inject({ method: 'GET', url: '/api/model-providers' })).statusCode).toBe(401);
}, 20000);
