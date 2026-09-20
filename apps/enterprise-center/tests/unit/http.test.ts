import { afterAll, beforeAll, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { createOpenApiDocument } from '../../dist/openapi.js';
import { createHttpApp } from '../../dist/app.js';

let app: Awaited<ReturnType<typeof createHttpApp>>;
beforeAll(async () => { app = await createHttpApp(); });
afterAll(async () => { await app.close(); });

it('GET /api/info 只暴露软件与协议兼容事实', async () => {
  const response = await app.inject({ method: 'GET', url: '/api/info' });
  expect(response.statusCode).toBe(200);
  expect(response.json()).toEqual({ software_version: '0.1.0', protocol_version: 1, min_protocol_version: 1 });
  expect(response.headers['cache-control']).toBe('no-store');
});

it('不存在的接口不回显 URL 或客户端 request id', async () => {
  const response = await app.inject({ method: 'GET', url: '/missing?token=sensitive', headers: { 'x-request-id': 'sensitive' } });
  expect(response.statusCode).toBe(404);
  expect(response.body).not.toContain('sensitive');
  expect(response.json().error.request_id).toMatch(/^[a-f0-9-]{36}$/);
});

it('JSON 解析失败不会回显原始输入', async () => {
  const response = await app.inject({ method: 'POST', url: '/api/info', headers: { 'content-type': 'application/json' }, payload: '{"sensitive"' });
  expect(response.statusCode).toBe(400);
  expect(response.json().error.code).toBe('INVALID_REQUEST');
  expect(response.body).not.toContain('sensitive');
});

it('请求体超过 16 KiB 有界拒绝', async () => {
  const response = await app.inject({ method: 'POST', url: '/api/info', headers: { 'content-type': 'application/json' }, payload: JSON.stringify({ value: 'x'.repeat(17000) }) });
  expect(response.statusCode).toBe(413);
  expect(response.json().error.code).toBe('INVALID_REQUEST');
});

it('OpenAPI 在线与离线快照一致，只包含已实现接口', async () => {
  const response = await app.inject({ method: 'GET', url: '/api/openapi.json' });
  expect(response.statusCode).toBe(200);
  const document = createOpenApiDocument(app);
  expect(response.json()).toEqual(document);
  expect(response.body).not.toMatch(/password_hash|CENTER_DATABASE_URL|123456/);
  expect(document).toEqual(JSON.parse(readFileSync('openapi.json', 'utf8')));
  expect(Object.keys(document.paths).sort()).toEqual(['/api/audit', '/api/auth/login', '/api/auth/logout', '/api/auth/me', '/api/auth/password', '/api/info', '/api/users', '/api/users/{id}', '/api/users/{id}/reset-password']);
  expect(document.paths['/api/info']?.get).toMatchObject({ operationId: 'getCenterInfo', responses: {
    '200': { content: { 'application/json': { schema: { $ref: '#/components/schemas/CenterInfo' } } } },
    default: { content: { 'application/json': { schema: { $ref: '#/components/schemas/CenterErrorResponse' } } } },
  } });
  expect(document.components?.schemas?.CenterInfo).toMatchObject({
    required: ['software_version', 'protocol_version', 'min_protocol_version'],
    properties: { software_version: { type: 'string' }, protocol_version: { type: 'integer', minimum: 1 }, min_protocol_version: { type: 'integer', minimum: 1 } },
  });
  expect(document.components?.schemas?.CenterErrorDetail).toMatchObject({
    required: ['code', 'message', 'request_id'], properties: { request_id: { type: 'string', format: 'uuid' } },
  });
});

it('Swagger 页面、初始化脚本和本地静态资源可访问，不启用外部校验或凭据持久化', async () => {
  const page = await app.inject({ method: 'GET', url: '/api/docs' });
  expect(page.statusCode).toBe(200);
  expect(page.headers['content-type']).toContain('text/html');
  expect(page.body).toContain('swagger-ui');
  const init = await app.inject({ method: 'GET', url: '/api/docs/swagger-ui-init.js' });
  expect(init.statusCode).toBe(200);
  expect(init.body).toContain('"validatorUrl": null');
  expect(init.body).toContain('"persistAuthorization": false');
  for (const asset of ['swagger-ui.css', 'swagger-ui-bundle.js']) {
    const response = await app.inject({ method: 'GET', url: `/api/docs/${asset}` });
    expect(response.statusCode).toBe(200);
  }
  expect((await app.inject({ method: 'GET', url: '/api/docs-yaml' })).statusCode).toBe(404);
});
