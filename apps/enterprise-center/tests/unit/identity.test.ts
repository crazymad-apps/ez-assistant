import { expect, it } from 'vitest';
import { Passwords } from '../../dist/identity/password.js';
import { fields } from '../../dist/validation.js';
import { loginInput, LoginLimiter, passwordInput } from '../../dist/identity/validation.js';
import { createHttpApp } from '../../dist/app.js';
import { createOpenApiDocument } from '../../dist/openapi.js';

it('新密码至少六位且包含英文和数字，历史密码仍可登录，密码不裁剪', () => {
  expect(loginInput({ username: ' ADMIN ', password: '123456' }).username).toBe('admin');
  for (const value of ['abc123', 'ABC123', '  spaces-kept1  ', '中😀 a1b', `a1${'x'.repeat(126)}`])
    expect(passwordInput({ old_password: '123456', new_password: value }).new_password).toBe(value);
  for (const value of ['ab123', 'abcdef', '123456', '中文数字123', '😀😀a1', `a1${'x'.repeat(127)}`])
    expect(() => passwordInput({ old_password: '123456', new_password: value })).toThrow();
  expect(() => passwordInput({ old_password: '', new_password: 'abc123' })).toThrow();
  expect(() => passwordInput({ old_password: '123456', new_password: '界'.repeat(129) })).toThrow();
});
it.each([null, [], { username: 'admin' }, { username: 'admin', password: '123456', role: 'admin' },
  { username: 'admin', password: ['123456'] }])('拒绝未知字段与错误输入 %j', value => {
  expect(() => loginInput(value)).toThrow();
});
it('空请求体只允许普通空对象', () => { expect(fields({}, [])).toEqual({}); expect(() => fields([], [])).toThrow(); });

it('真实 scrypt 摘要可验证且盐不同，不识别的参数不触发任意计算', async () => {
  const passwords = new Passwords();
  const first = await passwords.hash('safe-password-123');
  expect(await passwords.verify('safe-password-123', first)).toBe(true);
  expect(await passwords.verify('wrong', first)).toBe(false);
  expect(await passwords.verify('123456')).toBe(false);
  expect(await passwords.hash('safe-password-123')).not.toBe(first);
  await expect(passwords.verify('x', 'scrypt$99999999$8$1$broken')).rejects.toMatchObject({ code: 'SERVICE_UNAVAILABLE' });
});
it('密码计算容量有界，超出两个计算加八个等待时拒绝，完成后可继续', async () => {
  const passwords = new Passwords();
  const results = await Promise.allSettled(Array.from({ length: 11 }, () => passwords.verify('test')));
  expect(results.filter(value => value.status === 'fulfilled')).toHaveLength(10);
  const rejected = results.filter(value => value.status === 'rejected');
  expect(rejected).toHaveLength(1);
  expect(rejected[0]).toMatchObject({ reason: { code: 'RATE_LIMITED' } });
  expect(await passwords.verify('test')).toBe(false);
}, 10000);
it('限流按来源每分钟20次，状态最多1024项，过期后释放', () => {
  const limiter = new LoginLimiter();
  for (let i = 0; i < 20; i++) limiter.take('one', 0);
  expect(() => limiter.take('one', 1)).toThrow();
  for (let i = 0; i < 1023; i++) limiter.take(String(i), 0);
  expect(() => limiter.take('new', 1)).toThrow();
  expect(() => limiter.take('new', 60000)).not.toThrow();
});
it('OpenAPI 准确定义统一 Bearer 身份契约，离线装配不提供身份成功占位', async () => {
  const app = await createHttpApp();
  try {
    const document = createOpenApiDocument(app);
    expect(document.paths['/api/auth/login']?.post?.operationId).toBe('login');
    expect(document.security).toEqual([{ tokenBearer: [] }]);
    expect(document.paths['/api/auth/login']?.post?.security).toEqual([]);
    expect(Object.keys(document.components?.securitySchemes ?? {})).toEqual(['tokenBearer']);
    expect(document.components?.schemas?.LoginResponse).toMatchObject({ required: expect.arrayContaining(['token', 'llm_key']) });
    expect(document.components?.schemas?.IdentityResponse).not.toHaveProperty('properties.token');
    const response = await app.inject({ method: 'POST', url: '/api/auth/login', payload: { username: 'admin', password: '123456' } });
    expect(response.statusCode).toBe(503);
    expect(response.body).not.toContain('123456');
  } finally { await app.close(); }
});
