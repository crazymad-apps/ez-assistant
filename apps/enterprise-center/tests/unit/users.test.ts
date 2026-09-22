import { expect, it } from 'vitest';
import { createUserInput, resetPasswordInput, updateUserInput, userQuery } from '../../dist/users/validation.js';
import { recordId } from '../../dist/validation.js';
import { auditDetails, auditQuery } from '../../dist/audit/service.js';
import { createHttpApp } from '../../dist/app.js';
import { createOpenApiDocument } from '../../dist/openapi.js';
import { audits } from '../../dist/audit/repository.js';
import type { EntityManager } from 'typeorm';

const input = {
  username: ' Alice ',
  display_name: ' Alice ',
  role: 'user',
  enabled: true,
  password: 'safe-password-123',
};
it('创建与管理重置共用六位英文数字规则', () => {
  expect(createUserInput({ ...input, password: 'abc123' }).password).toBe('abc123');
  expect(resetPasswordInput({ new_password: 'ABC123' })).toBe('ABC123');
  expect(() => resetPasswordInput({ new_password: 'abcdef' })).toThrow();
});
it('创建账号归一化；不接受标记、ID 或未知字段', () => {
  expect(createUserInput(input)).toEqual({ ...input, username: 'alice', display_name: 'Alice' });
  for (const extra of [{ is_super_admin: true }, { id: 'x' }, { token: 'x' }])
    expect(() => createUserInput({ ...input, ...extra })).toThrow();
  expect(() => createUserInput({ ...input, password: '123456' })).toThrow();
});
it.each([
  {},
  { username: 'other' },
  { is_super_admin: false },
  { enabled: 'false' },
  { role: 'super' },
  { display_name: '   ' },
  { display_name: 'bad\nname' },
  { display_name: '界'.repeat(65) },
])('编辑拒绝无效/越权字段 %j', (body) => {
  expect(() => updateUserInput(body)).toThrow();
});
it('PATCH 只修改显式字段，管理重置不收旧密码', () => {
  expect(updateUserInput({ enabled: false })).toEqual({ enabled: false });
  expect(resetPasswordInput({ new_password: 'new-password-123' })).toBe('new-password-123');
  expect(() => resetPasswordInput({ new_password: 'new-password-123', old_password: 'old' })).toThrow();
  expect(recordId('1')).toBe(1);
  expect(recordId('2147483647')).toBe(2147483647);
  for (const value of [
    '0',
    '-1',
    '01',
    '1.0',
    '1e2',
    ' 1',
    '2147483648',
    '9007199254740993',
    1,
    null,
    ['1'],
    'AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE',
  ])
    expect(() => recordId(value)).toThrow();
});
it('用户查询只接受单值、有界分页和精确布尔值', () => {
  expect(userQuery({})).toEqual({ limit: 20, offset: 0 });
  expect(userQuery({ search: ' %_', role: 'admin', enabled: 'false', limit: '100', offset: '1000000' })).toEqual({
    search: '%_',
    role: 'admin',
    enabled: false,
    limit: 100,
    offset: 1000000,
  });
  for (const query of [
    { limit: '0' },
    { limit: '101' },
    { limit: '1e2' },
    { limit: ['1', '2'] },
    { offset: '-1' },
    { offset: '1000001' },
    { enabled: '1' },
    { enabled: false },
    { search: 'x'.repeat(65) },
    { unknown: 'x' },
  ])
    expect(() => userQuery(query)).toThrow();
});
it('审计查询拒绝非法日期、倒序区间与重复参数', () => {
  expect(auditQuery({ from: '2026-09-18T00:00:00Z', to: '2026-09-19T00:00:00.000Z', success: 'true' })).toMatchObject({
    success: true,
    limit: 20,
  });
  for (const query of [
    { from: '2026-02-30T00:00:00Z' },
    { to: 'not-date' },
    { from: '2026-09-18T00:00:00Z', to: '2026-09-18T00:00:00Z' },
    { action: ['login'] },
    { actor_user_id: 'x' },
  ])
    expect(() => auditQuery(query)).toThrow();
});
it('审计详情 ID 校验并参数化传入现有只读查询', async () => {
  const id = '123';
  expect(auditQuery({ id })).toMatchObject({ id: Number(id) });
  for (const invalidId of ['x', [id, id], "' OR true --"]) expect(() => auditQuery({ id: invalidId })).toThrow();
  const calls: unknown[][] = [];
  const manager = {
    query: async (...args: unknown[]) => {
      calls.push(args);
      return [{ total: 0, items: [] }];
    },
  } as unknown as EntityManager;
  expect(await audits.list(manager, auditQuery({ id }))).toEqual({ total: 0, items: [] });
  expect(calls[0]?.[0]).toContain('id=$8');
  expect(calls[0]?.[1]).toEqual([null, null, null, null, null, 20, 0, Number(id)]);
});
it('审计详情只投影普通字段，不回传任意 JSON 或秘密', () => {
  expect(
    auditDetails({
      password: 'secret',
      before: { display_name: 'Alice', role: 'user', password_hash: 'secret' },
      after: { enabled: false, token: 'secret', nested: { password: 'secret' } },
    }),
  ).toEqual({ before: { display_name: 'Alice', role: 'user' }, after: { enabled: false } });
  expect(auditDetails(null)).toEqual({});
});
it('OpenAPI 描述真实管理接口、分页字段和只读超级管理员标记', async () => {
  const app = await createHttpApp();
  try {
    const document = createOpenApiDocument(app);
    const numericId = { type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 };
    expect(document.components?.schemas?.IdentityUser).toMatchObject({ properties: { id: numericId } });
    expect(document.components?.schemas?.AuditRecord).toMatchObject({
      properties: {
        id: numericId,
        actor_user_id: { ...numericId, nullable: true },
        target_user_id: { ...numericId, nullable: true },
        actor_username: { type: 'string', nullable: true },
        target_username: { type: 'string', nullable: true },
      },
    });
    expect(document.paths['/api/users/{id}']?.patch?.parameters).toEqual(
      expect.arrayContaining([expect.objectContaining({ name: 'id', in: 'path', schema: numericId })]),
    );
    expect(document.paths['/api/users']?.post?.operationId).toBe('createUser');
    expect(document.paths['/api/users/{id}/reset-password']?.post?.operationId).toBe('resetUserPassword');
    expect(document.paths['/api/users']?.get?.parameters).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ name: 'limit', in: 'query', required: false }),
        expect.objectContaining({ name: 'search', in: 'query' }),
      ]),
    );
    expect(document.paths['/api/audit']?.get?.parameters).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ name: 'actor_user_id' }),
        expect.objectContaining({ name: 'id', in: 'query', required: false }),
      ]),
    );
    for (const schema of ['CreateUserRequest', 'UpdateUserRequest'])
      expect(document.components?.schemas?.[schema]).not.toHaveProperty('properties.is_super_admin');
    expect(document.paths['/api/users/{id}']).not.toHaveProperty('delete');
    expect(document.components?.schemas?.ResetPasswordRequest).toMatchObject({ required: ['new_password'] });
    expect((await app.inject({ method: 'GET', url: '/api/users' })).statusCode).toBe(503);
  } finally {
    await app.close();
  }
});

it('模型审计只投影目标与计数，不能返回嵌入的连接及密钥', () => {
  const provider_instance_id = '4614318a-c1f1-4ae5-af77-1784d1e7a526';
  expect(
    auditDetails({
      provider_instance_id,
      model_id: 'real/id',
      model_count: 1,
      api_key: 'secret',
      endpoint: 'secret',
      default_model: { provider_instance_id, model_id: 'real/id', api_key: 'secret' },
    }),
  ).toEqual({
    provider_instance_id,
    model_id: 'real/id',
    model_count: 1,
    default_model: { provider_instance_id, model_id: 'real/id' },
  });
});
