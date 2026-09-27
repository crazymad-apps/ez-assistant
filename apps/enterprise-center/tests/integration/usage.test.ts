import { mkdtemp } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createServer } from 'node:http';
import { once } from 'node:events';
import type { AddressInfo } from 'node:net';
import { expect, it } from 'vitest';
import { createDataSource } from '../../dist/database/source.js';
import { createHttpApp } from '../../dist/app.js';
import { CallEntity, RecordingEntity } from '../../dist/llm/entity.js';

const url = process.env.CENTER_TEST_DATABASE_URL;
if (!url || process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true')
  throw new Error('F06 隔离库备份未核验');
const target = new URL(url);
if (target.hostname !== '127.0.0.1' || target.port !== '55441' || target.pathname !== '/ez_center_f06_retry_acceptance' || target.username !== 'center_app')
  throw new Error('F06 隔离库目标不符');

it('代理用量与正文策略独立，联合模型维度和调用明细一致', async () => {
  const source = createDataSource({ url });
  const snapshotRoot = await mkdtemp(join(resolve('../../.runtime-test/f06-usage-20260927'), 'snapshots-'));
  const received: string[] = [];
  let nextStatus = 200;
  const upstream = createServer(async (request, response) => {
    let raw = '';
    for await (const chunk of request) raw += chunk.toString();
    received.push(raw);
    if (request.method === 'GET') {
      response.writeHead(200, { 'content-type': 'application/json' });
      response.end('{"object":"list","data":[{"id":"gpt-6-astra","object":"model"}]}');
      return;
    }
    response.writeHead(nextStatus, { 'content-type': 'application/json' });
    nextStatus = 200;
    response.end(JSON.stringify({
      object: 'response', output: [{ content: [{ type: 'output_text', text: 'OK' }] }],
      usage: { input_tokens: 12, output_tokens: 3, total_tokens: 15,
        input_tokens_details: { cached_tokens: 4 }, output_tokens_details: { reasoning_tokens: 1 } },
    }));
  });
  upstream.listen(0, '127.0.0.1');
  await once(upstream, 'listening');
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  try {
    await source.initialize();
    const identity = await source.query("SELECT host(inet_server_addr()) AS host, inet_server_port() AS port, current_database() AS db, current_user AS role");
    expect(identity[0]).toMatchObject({ host: '127.0.0.1', port: 55441, db: 'ez_center_f06_retry_acceptance', role: 'center_app' });
    expect(await source.getRepository(CallEntity).count()).toBe(0);
    expect((await source.query('SELECT COUNT(*)::integer AS n FROM public.providers'))[0].n).toBe(0);
    const settings = await source.getRepository(RecordingEntity).findOneByOrFail({ singleton: true });
    expect(settings.enabled).toBe(false);
    app = await createHttpApp({ source, origin: 'http://127.0.0.1:17330', snapshotRoot });
    await app.listen(0, '127.0.0.1');
    const endpoint = await app.getUrl();
    const login = await fetch(`${endpoint}/api/auth/login`, {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password: '123456' }),
    });
    expect(login.status).toBe(200);
    const credentials = await login.json() as { token: string; llm_key: string };
    const api = async (method: string, path: string, body?: object) => {
      const response = await fetch(`${endpoint}/api/${path}`, {
        method, headers: { authorization: `Bearer ${credentials.token}`, ...(body ? { 'content-type': 'application/json' } : {}) },
        body: body ? JSON.stringify(body) : undefined,
      });
      expect(response.status, `${method} ${path}`).toBeGreaterThanOrEqual(200);
      expect(response.status, `${method} ${path}`).toBeLessThan(300);
      return response.json();
    };
    const createProvider = async (name: string) => {
      const provider = await api('POST', 'model-providers', {
        connection: { display_name: name, provider_type: 'openai',
          endpoint: `http://127.0.0.1:${(upstream.address() as AddressInfo).port}/v1`,
          protocol_preference: 'responses', models_path: '', discovery_format: 'openai' },
        credential: { mode: 'replace', value: 'test-secret' },
      });
      const selection = { provider_instance_id: provider.provider_instance_id as string, model_id: 'gpt-6-astra' };
      await api('POST', `model-providers/${selection.provider_instance_id}/refresh`, {});
      const config = await api('POST', 'model-configuration/query', { ...selection, origin: 'online' });
      const parameters = {
        ...config.parameters,
        context_window_tokens: { state: 'known', value: 256000 },
        max_output_tokens: { state: 'known', value: 128000 },
        streaming: 'supported',
      };
      await api('PUT', 'model-configuration', { ...selection, origin: 'online', parameters });
      return selection;
    };
    const first = await createProvider('渠道 A');
    const second = await createProvider('渠道 B');
    const invoke = async (providerId: string, expectedStatus = 200) => {
      const raw = '{"model":"gpt-6-astra","input":"hello","stream":false}';
      const response = await fetch(`${endpoint}/api/llm/providers/${providerId}/v1/responses`, {
        method: 'POST', headers: { authorization: `Bearer ${credentials.llm_key}`, 'content-type': 'application/json' }, body: raw,
      });
      expect(response.status).toBe(expectedStatus);
      const id = response.headers.get('x-ez-call-id')!;
      await response.text();
      for (let attempt = 0; attempt < 100; attempt++) {
        const row = await source.getRepository(CallEntity).findOneByOrFail({ id });
        if (row.usage_state !== 'pending' && row.response_snapshot_state !== 'pending') return row;
        await new Promise((done) => setTimeout(done, 20));
      }
      throw new Error('usage_settlement_timeout');
    };
    await api('PUT', 'model-settings', { default_model: first });
    const withoutBody = await invoke(first.provider_instance_id);
    expect(withoutBody.usage_state).toBe('available');
    expect(withoutBody.total_tokens).toBe('15');
    expect(withoutBody.request_snapshot_state).toBe('disabled');
    await api('PUT', 'llm-recording-settings', { enabled: true, content_retention_days: 7, index_retention_days: 90 });
    await api('PUT', 'model-settings', { default_model: second });
    const withBody = await invoke(second.provider_instance_id);
    expect(withBody.request_snapshot_state).toBe('complete');
    expect(withBody.response_snapshot_state).toBe('complete');
    const snapshot = await api('GET', `llm-calls/${withBody.id}/snapshot?side=response`);
    expect(snapshot.text).toContain('"total_tokens":15');
    const usage = await api('GET', 'llm-usage?granularity=hour&group_by=model&outcome=forwarded');
    expect(usage.summary).toMatchObject({ calls: '2', available: '2', total_tokens: '30', cached_input_tokens: '8' });
    expect(usage.groups).toHaveLength(2);
    expect(usage.groups.map((item: { provider_instance_id: string }) => item.provider_instance_id).sort())
      .toEqual([first.provider_instance_id, second.provider_instance_id].sort());
    const all = await api('GET', 'llm-usage?granularity=hour&group_by=model');
    expect(all.summary).toMatchObject({ calls: '2', available: '2', unavailable: '0', total_tokens: '30' });
    const filtered = await api('GET', `llm-usage?granularity=day&group_by=model&provider_instance_id=${first.provider_instance_id}&model_id=gpt-6-astra&outcome=forwarded`);
    expect(filtered.summary).toMatchObject({ calls: '1', total_tokens: '15' });
    const details = await api('GET', `llm-calls/${withoutBody.id}`);
    expect(details).toMatchObject({ usage_state: 'available', total_tokens: '15', request: { state: 'disabled' } });
    expect(received.filter((body) => body.includes('"input":"hello"'))).toHaveLength(2);
    await api('PUT', 'model-settings', { default_model: first });
    nextStatus = 503;
    const failedAttempt = await invoke(first.provider_instance_id, 503);
    const retryAttempt = await invoke(first.provider_instance_id);
    expect(failedAttempt.id).not.toBe(retryAttempt.id);
    expect(failedAttempt).toMatchObject({ outcome: 'upstream_error', usage_state: 'available', total_tokens: '15' });
    expect(retryAttempt).toMatchObject({ outcome: 'forwarded', usage_state: 'available', total_tokens: '15' });
    const retries = await api('GET', `llm-usage?outcome=upstream_error&provider_instance_id=${first.provider_instance_id}&model_id=gpt-6-astra`);
    expect(retries.summary).toMatchObject({ calls: '1', total_tokens: '15' });
    const afterRetry = await api('GET', 'llm-usage?granularity=day');
    expect(afterRetry.summary).toMatchObject({ calls: '4', available: '4', total_tokens: '60', forwarded: '3', upstream_error: '1' });
  } finally {
    await app?.close();
    if (source.isInitialized) await source.destroy();
    upstream.closeAllConnections();
    await new Promise<void>((done) => upstream.close(() => done()));
  }
}, 30000);
