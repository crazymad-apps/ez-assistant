import { expect, it, vi } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { IdentityError } from '../../dist/identity/errors.js';
import { ModelProxy } from '../../dist/llm/proxy.js';

it('调用记录、正文和策略接口全部拒绝普通用户；raw scope 的鉴权拒绝携带控制头', async () => {
  const app = await createHttpApp();
  const identity = app.get(IdentityService);
  vi.spyOn(identity, 'me').mockResolvedValue({
    center_id: 'test-center',
    user: { id: 1, username: 'user', display_name: 'user', role: 'user', is_super_admin: false, enabled: true },
  });
  vi.spyOn(identity, 'llmIdentity').mockRejectedValue(new IdentityError(401, 'TOKEN_INVALID'));
  try {
    for (const url of [
      '/api/llm-calls',
      '/api/llm-calls/00000000-0000-0000-0000-000000000001',
      '/api/llm-calls/00000000-0000-0000-0000-000000000001/snapshot?side=request',
      '/api/llm-recording-settings',
    ]) {
      const response = await app.inject({ method: 'GET', url, headers: { authorization: 'Bearer ct_test' } });
      expect(response.statusCode).toBe(403);
    }
    const rejected = await app.inject({
      method: 'POST',
      url: '/api/llm/providers/00000000-0000-0000-0000-000000000001/v1/responses',
      headers: { authorization: 'Bearer invalid', 'content-type': 'application/json',
        origin: 'https://client.example', 'sec-fetch-site': 'cross-site' },
      payload: '{"model":"m"}',
    });
    expect(rejected.statusCode).toBe(401);
    expect(rejected.headers['x-ez-center-control']).toBe('1');
    expect(rejected.json().error.code).toBe('llm_key_invalid');
    expect(rejected.headers['access-control-allow-origin']).toBe('*');
    expect(rejected.headers['access-control-expose-headers']).toContain('X-EZ-Center-Control');
  } finally {
    await app.close();
    vi.restoreAllMocks();
  }
});

it('raw 流式代理保留 CORS 响应头且上游不能覆盖跨域策略', async () => {
  const app = await createHttpApp();
  const identity = app.get(IdentityService);
  const user = { id: 1, username: 'user', display_name: 'user', role: 'user' as const, is_super_admin: false, enabled: true };
  vi.spyOn(identity, 'llmIdentity').mockResolvedValue({ center_id: 'test-center', user });
  const payload = 'data: {"type":"response.completed"}\n\n';
  vi.spyOn(app.get(ModelProxy), 'execute').mockImplementation(async input => {
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
