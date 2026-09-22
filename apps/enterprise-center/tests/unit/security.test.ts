import 'reflect-metadata';
import { expect, it, vi } from 'vitest';
import type { ExecutionContext } from '@nestjs/common';
import { ModelsController } from '../../dist/models/controller.js';
import { Access, HttpSecurity } from '../../dist/security.js';
import type { AuthenticatedRequest } from '../../dist/security.js';
import { IdentityService } from '../../dist/identity/service.js';
import { IdentityError } from '../../dist/identity/errors.js';

const origin = 'https://center.example';

function fixture(mode?: 'public' | 'optional' | 'admin', method = 'GET') {
  class Endpoint {}
  if (mode) Access(mode)(Endpoint);
  const request = { method, headers: {}, id: 'request' } as AuthenticatedRequest;
  const context = {
    getHandler: () => function handler() {},

    getClass: () => Endpoint,

    switchToHttp: () => ({ getRequest: () => request }),
  } as unknown as ExecutionContext;
  const identity = new IdentityService();
  const user = {
    id: 1,
    username: 'alice',
    display_name: 'Alice',
    enabled: true,
    role: 'user' as const,
    is_super_admin: false,
  };
  const me = vi.spyOn(identity, 'me').mockImplementation(async (token) => {
    if (token !== 'valid') throw new IdentityError(401, 'TOKEN_INVALID');
    return { center_id: 'center', user };
  });
  return { context, request, me, user, guard: new HttpSecurity(identity, origin) };
}

it('默认接口必须登录；只接受 Bearer，不从 Cookie 或 URL 取 Token', async () => {
  const f = fixture();
  f.request.headers = { cookie: 'token=valid' };
  await expect(f.guard.canActivate(f.context)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  f.request.headers.authorization = 'Bearer valid';
  await expect(f.guard.canActivate(f.context)).resolves.toBe(true);
  expect(f.request.identity?.user.id).toBe(1);
  f.request.headers.authorization = 'Basic valid';
  await expect(f.guard.canActivate(f.context)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
});

it.each(['public', 'optional'] as const)('%s 放行认证但不跳过来源与 JSON 检查', async (mode) => {
  const f = fixture(mode);
  await expect(f.guard.canActivate(f.context)).resolves.toBe(true);
  expect(f.me).not.toHaveBeenCalled();
  f.request.headers.origin = 'https://evil.example';
  await expect(f.guard.canActivate(f.context)).rejects.toMatchObject({ code: 'FORBIDDEN' });
  const post = fixture(mode, 'POST');
  post.request.headers = { origin };
  await expect(post.guard.canActivate(post.context)).rejects.toMatchObject({ code: 'INVALID_REQUEST' });
  post.request.headers['content-type'] = 'application/json';
  await expect(post.guard.canActivate(post.context)).resolves.toBe(true);
  post.request.headers['sec-fetch-site'] = 'cross-site';
  await expect(post.guard.canActivate(post.context)).rejects.toMatchObject({ code: 'FORBIDDEN' });
});

it('管理员规则读取用户权限，超级管理员标记同样生效', async () => {
  const f = fixture('admin');
  f.request.headers.authorization = 'Bearer valid';
  await expect(f.guard.canActivate(f.context)).rejects.toMatchObject({ code: 'ADMIN_REQUIRED' });
  f.me.mockResolvedValue({ center_id: 'center', user: { ...f.user, role: 'admin' } });
  await expect(f.guard.canActivate(f.context)).resolves.toBe(true);
  f.me.mockResolvedValue({ center_id: 'center', user: { ...f.user, is_super_admin: true } });
  await expect(f.guard.canActivate(f.context)).resolves.toBe(true);
});

it('模型管理拒绝普通用户，Runtime 配置接口允许已登录用户', async () => {
  const f = fixture();
  f.request.headers.authorization = 'Bearer valid';
  const context = {
    ...f.context,

    getClass: () => ModelsController,

    getHandler: () => ModelsController.prototype.list,
  } as ExecutionContext;
  await expect(f.guard.canActivate(context)).rejects.toMatchObject({ code: 'ADMIN_REQUIRED' });
  await expect(
    f.guard.canActivate({ ...context, getHandler: () => ModelsController.prototype.runtime } as ExecutionContext),
  ).resolves.toBe(true);
});
