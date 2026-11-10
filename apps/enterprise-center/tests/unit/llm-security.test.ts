import { expect, it, vi } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { IdentityError } from '../../dist/identity/errors.js';
import { IdentityService } from '../../dist/identity/service.js';
import Fastify from 'fastify';
import cors from '@fastify/cors';
import { CallRecords } from '../../dist/llm/records.js';
import { SnapshotFiles } from '../../dist/llm/snapshots.js';
import { ModelsService } from '../../dist/models/service.js';
import { ModelTemplates } from '../../dist/models/templates.js';
import { ModelProxy } from '../../dist/llm/proxy.js';

it('调用记录、正文、策略及 raw 模型入口分别校验对应凭据', async () => {
  const app = await createHttpApp();
  const identity = app.get(IdentityService);
  vi.spyOn(identity, 'me').mockRejectedValue(new IdentityError(401, 'TOKEN_INVALID'));
  vi.spyOn(identity, 'llmIdentity').mockRejectedValue(new IdentityError(401, 'TOKEN_INVALID'));
  try {
    for (const url of [
      '/api/llm-calls',
      '/api/llm-calls/00000000-0000-0000-0000-000000000001',
      '/api/llm-calls/00000000-0000-0000-0000-000000000001/snapshot?side=request',
      '/api/llm-recording-settings',
    ]) {
      const response = await app.inject({ method: 'GET', url, headers: { authorization: 'Bearer ct_test' } });
      expect(response.statusCode).toBe(401);
    }
    const rejected = await app.inject({
      method: 'POST',
      url: '/api/llm/providers/00000000-0000-0000-0000-000000000001/v1/responses',
      headers: { authorization: 'Bearer invalid', 'content-type': 'application/json',
        origin: 'https://client.example', 'sec-fetch-site': 'cross-site' },
      payload: '{"model":"m"}',
    });
    expect(rejected.statusCode).toBe(401);
    expect(rejected.headers['access-control-allow-origin']).toBe('*');
    expect(rejected.headers['access-control-expose-headers']).toContain('X-EZ-Center-Control');
  } finally {
    await app.close();
    vi.restoreAllMocks();
  }
});

it('raw 流式代理保留 CORS 响应头且上游不能覆盖跨域策略', async () => {
  const app = Fastify();
  await app.register(cors, { origin: '*', exposedHeaders: ['X-EZ-Call-Id'] });
  const identity = new IdentityService();
  const proxy = new ModelProxy(identity, new ModelsService(identity, undefined, new ModelTemplates()),
    new CallRecords(undefined, new SnapshotFiles()), {});
  proxy.install(app);
  const user = { id: 1, username: 'user', display_name: 'user', role: 'user' as const, is_super_admin: false, enabled: true };
  vi.spyOn(identity, 'llmIdentity').mockResolvedValue({ center_id: 'test-center', legacy_organization_id: 'legacy', user,
    scope: { kind: 'organization', organization_id: 'legacy', can_manage: false },
    session: { jti: 'test', userId: 1, issuer: 'test-center', scope: { kind: 'organization', organization_id: 'legacy' }, expires: 4070908800 },
    expires_at: '2099-01-01T00:00:00Z' });
  const payload = 'data: {"type":"response.completed"}\n\n';
  vi.spyOn(proxy, 'execute').mockImplementation(async input => {
    input.sent?.();
    input.sink.headers(200, { 'content-type': 'text/event-stream',
      'access-control-allow-origin': 'https://upstream.example', 'access-control-allow-credentials': 'true' });
    await input.sink.chunk(Buffer.from(payload), input.signal);
    await input.finish?.(input.signal);
    return { call_id: 'test-call', http_status: 200 };
  });
  try {
    const response = await app.inject({ method: 'POST',
      url: '/api/llm/providers/00000000-0000-0000-0000-000000000001/v1/responses',
      headers: { authorization: 'Bearer cl_test', 'content-type': 'application/json',
        origin: 'https://client.example', 'sec-fetch-site': 'cross-site' },
      payload: '{"model":"m","stream":true,"store":false}',
    });
    expect(response.statusCode).toBe(200);
    expect(response.headers['content-type']).toBe('text/event-stream');
    expect(response.headers['access-control-allow-origin']).toBe('*');
    expect(response.headers['access-control-allow-credentials']).toBeUndefined();
    expect(response.headers['access-control-expose-headers']).toContain('X-EZ-Call-Id');
    expect(response.body).toBe(payload);
  } finally {
    await app.close();
    vi.restoreAllMocks();
  }
});
