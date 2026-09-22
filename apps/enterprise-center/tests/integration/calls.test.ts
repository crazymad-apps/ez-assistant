import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createServer } from 'node:http';
import { once } from 'node:events';
import type { AddressInfo } from 'node:net';
import { expect, it } from 'vitest';
import { createDataSource } from '../../dist/database/source.js';
import { createHttpApp } from '../../dist/app.js';
import { ModelProxy } from '../../dist/llm/proxy.js';
import { CallEntity } from '../../dist/llm/entity.js';

const url = process.env.CENTER_TEST_DATABASE_URL;
if (!url || process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true')
  throw new Error('需要已恢复核验的独立 C04 M1 库');
const target = new URL(url);
if (target.host !== '127.0.0.1:55435' || target.pathname !== '/ez_center_c04_m1' || target.username !== 'center_app')
  throw new Error('wrong isolated target');

it('真实 HTTP 的代理、配置测试与快照管理闭环，启动恢复保留已确认转发结果', async () => {
  const root = resolve('../../.runtime-test/c04-m1-isolated-20260921');
  const snapshots = await mkdtemp(join(root, 'records-'));
  const source = createDataSource({ url });
  const received: Buffer[] = [];
  let mode = 'normal';
  const upstream = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(chunk);
    if (request.method === 'GET') {
      response.setHeader('content-type', 'application/json');
      response.end(JSON.stringify({ object: 'list', data: [{ id: 'gpt-6-astra', object: 'model' }] }));
      return;
    }
    received.push(Buffer.concat(chunks));
    expect(request.headers.authorization).toBe('Bearer test-provider-secret');
    expect(request.headers.cookie).toBeUndefined();
    if (mode === 'idle') return;
    response.writeHead(mode === 'error' ? 401 : 200, {
      'content-type': 'application/json',
      'x-ez-center-control': '1',
      'x-ez-call-id': 'spoofed',
      'set-cookie': 'untrusted',
    });
    if (mode === 'broken') {
      response.write('partial');
      setTimeout(() => response.destroy(), 20);
      return;
    }
    response.end(
      request.url?.endsWith('/responses')
        ? '{"object":"response","output":[{"content":[{"type":"output_text","text":"OK"}]}]}'
        : '{"choices":[{"message":{"role":"assistant","content":"OK"}}]}',
    );
  });
  upstream.listen(0, '127.0.0.1');
  await once(upstream, 'listening');
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  try {
    await source.initialize();
    expect(await source.getRepository(CallEntity).count()).toBe(0);
    app = await createHttpApp({
      source,
      origin: 'http://127.0.0.1:17330',
      snapshotRoot: snapshots,
      llmIdleTimeout: 1000,
    });
    await app.listen(0, '127.0.0.1');
    const endpoint = await app.getUrl();
    const login = await fetch(`${endpoint}/api/auth/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password: '123456' }),
    });
    expect(login.status).toBe(200);
    const identity = (await login.json()) as { token: string; llm_key: string };

    const request = async (method: string, path: string, body?: object) => {
      const response = await fetch(`${endpoint}/api/${path}`, {
        method,
        headers: { authorization: `Bearer ${identity.token}`, ...(body ? { 'content-type': 'application/json' } : {}) },
        body: body ? JSON.stringify(body) : undefined,
      });
      expect(response.ok, `${method} ${path}: ${response.status}`).toBe(true);
      return response.json();
    };

    const provider = await request('POST', 'model-providers', {
      connection: {
        display_name: 'M1 确定性上游',
        provider_type: 'openai',
        endpoint: `http://127.0.0.1:${(upstream.address() as AddressInfo).port}/v1`,
        protocol_preference: 'responses',
        models_path: '',
        discovery_format: 'openai',
      },
      credential: { mode: 'replace', value: 'test-provider-secret' },
    });
    const selection = { provider_instance_id: provider.provider_instance_id, model_id: 'gpt-6-astra' };
    await request('POST', `model-providers/${selection.provider_instance_id}/refresh`, {});
    const config = await request('POST', 'model-configuration/query', { ...selection, origin: 'online' });
    await request('PUT', 'model-configuration', { ...selection, origin: 'online', parameters: config.parameters });
    await request('PUT', 'model-settings', { default_model: selection });
    await request('PUT', 'llm-recording-settings', {
      enabled: true,
      content_retention_days: 7,
      index_retention_days: 90,
    });
    const proxyUrl = `${endpoint}/api/llm/providers/${selection.provider_instance_id}/v1/responses`;
    const raw = '{ "model":"gpt-6-astra", "input":"hello", "unknown":9007199254740993 }';

    const post = (body = raw, key = identity.llm_key) =>
      fetch(proxyUrl, {
        method: 'POST',
        headers: { authorization: `Bearer ${key}`, 'content-type': 'application/json', cookie: 'private-cookie' },
        body,
      });

    const response = await post();
    expect(response.status).toBe(200);
    expect(response.headers.get('x-ez-center-control')).toBeNull();
    expect(response.headers.get('set-cookie')).toBeNull();
    const callId = response.headers.get('x-ez-call-id')!;
    expect(callId).not.toBe('spoofed');
    const content = await response.text();
    expect(received[0]!.toString()).toBe(raw);

    // 响应 EOF 先于文件结算，等待该调用 owner 收尾，不把瞬时 pending 当作失败。
    async function settled(id: string) {
      for (let attempt = 0; attempt < 100; attempt++) {
        const row = await source.getRepository(CallEntity).findOneByOrFail({ id });
        if (
          row.outcome !== 'in_progress' &&
          row.request_snapshot_state !== 'pending' &&
          row.response_snapshot_state !== 'pending'
        )
          return row;
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      throw new Error('settlement_timeout');
    }

    const row = await settled(callId);
    expect(row.outcome).toBe('forwarded');
    expect(row.request_snapshot_state).toBe('complete');
    expect((await request('GET', `llm-calls/${callId}/snapshot?side=request`)).text).toBe(raw);
    expect((await request('GET', `llm-calls/${callId}/snapshot?side=response`)).text).toBe(content);
    const duplicate = await post('{"model":"gpt-6-astra","model":"other"}');
    expect(duplicate.status).toBe(400);
    expect(duplicate.headers.get('x-ez-center-control')).toBe('1');
    expect((await post(raw, identity.token)).status).toBe(401);
    const stale = await post('{"model":"other"}');
    expect(stale.status).toBe(409);
    const rejected = await settled(stale.headers.get('x-ez-call-id')!);
    expect(rejected.outcome).toBe('rejected');
    expect(received).toHaveLength(1);
    mode = 'error';
    const upstreamError = await post();
    expect(upstreamError.status).toBe(401);
    expect(upstreamError.headers.get('x-ez-center-control')).toBeNull();
    await upstreamError.text();
    expect((await settled(upstreamError.headers.get('x-ez-call-id')!)).outcome).toBe('upstream_error');
    mode = 'normal';
    const tested = await request('POST', 'model-configuration/test', selection);
    expect(tested.connected).toBe(true);
    expect((await request('GET', `llm-calls/${tested.call_id}`)).kind).toBe('admin_test');
    expect((await request('GET', 'llm-calls?limit=50')).items).toHaveLength(4);
    // 只改新隔离场景的文件，模拟索引完整、文件丢失/损坏；不触碰共享库或旧预览。
    await app.get(ModelProxy).close();
    await app.close();
    app = undefined;
    await writeFile(join(snapshots, row.response_path!), 'corrupted', { mode: 0o600 });
    app = await createHttpApp({ source, origin: 'http://127.0.0.1:17330', snapshotRoot: snapshots });
    const recovered = await source.getRepository(CallEntity).findOneByOrFail({ id: callId });
    expect(recovered.outcome).toBe('forwarded');
    expect(recovered.response_snapshot_state).toBe('failed');
    expect((await readFile(join(snapshots, row.request_path!))).toString()).toBe(raw);
  } finally {
    await app?.get(ModelProxy).close();
    await app?.close();
    if (source.isInitialized) await source.destroy();
    upstream.closeAllConnections();
    await new Promise<void>((resolve) => upstream.close(() => resolve()));
  }
}, 30000);
