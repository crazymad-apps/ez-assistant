import { expect, it, vi } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { IdentityError } from '../../dist/identity/errors.js';

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
      headers: { authorization: 'Bearer invalid', 'content-type': 'application/json' },
      payload: '{"model":"m"}',
    });
    expect(rejected.statusCode).toBe(401);
    expect(rejected.headers['x-ez-center-control']).toBe('1');
    expect(rejected.json().error.code).toBe('llm_key_invalid');
  } finally {
    await app.close();
    vi.restoreAllMocks();
  }
});
