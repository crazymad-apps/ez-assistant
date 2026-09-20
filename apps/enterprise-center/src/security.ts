import { SetMetadata } from '@nestjs/common';
import type { CanActivate, ExecutionContext } from '@nestjs/common';
import { Reflector } from '@nestjs/core';
import type { FastifyRequest } from 'fastify';
import type { IdentityResponse } from './contracts/identity.js';
import { IdentityError } from './identity/errors.js';
import type { IdentityService } from './identity/service.js';

type Policy = { mode: 'public' | 'optional' | 'authenticated' | 'admin'; action?: string };
const accessPolicy = Symbol('access-policy');
/** 未标记的 Nest 接口默认要求登录；optional 仅用于无效 Token 也能幂等退出。 */
export const Access = (mode: Policy['mode'], action?: string) => SetMetadata(accessPolicy, { mode, action } satisfies Policy);
export type AuthenticatedRequest = FastifyRequest & { token?: string; identity?: IdentityResponse };

/** 全局安全入口，不从 Cookie/URL 读取身份，不把通用鉴权分散到业务 Controller。 */
export class HttpSecurity implements CanActivate {
  private readonly reflector = new Reflector();
  constructor(private readonly identity: IdentityService, private readonly origin?: string) {}
  async canActivate(context: ExecutionContext): Promise<boolean> {
    const policy = this.reflector.getAllAndOverride<Policy>(accessPolicy, [context.getHandler(), context.getClass()]);
    const request = context.switchToHttp().getRequest<AuthenticatedRequest>();
    const check = async () => {
      if ((request.headers.origin !== undefined && request.headers.origin !== this.origin)
        || request.headers['sec-fetch-site'] === 'cross-site') throw new IdentityError(403, 'FORBIDDEN');
      if (['POST', 'PUT', 'PATCH'].includes(request.method)
        && !/^application\/json(?:\s*;|$)/i.test(request.headers['content-type'] ?? '')) throw new IdentityError(400, 'INVALID_REQUEST');
      request.token = /^Bearer ([^\s]+)$/i.exec(request.headers.authorization ?? '')?.[1];
      if (policy?.mode === 'public' || policy?.mode === 'optional') return true;
      request.identity = await this.identity.me(request.token);
      const user = request.identity.user;
      if (policy?.mode === 'admin' && !user.is_super_admin && user.role !== 'admin') throw new IdentityError(403, 'ADMIN_REQUIRED');
      return true;
    };
    // Guard 拒绝发生在 Controller 之前；沿用业务动作的脱敏失败审计，成功不在此重复记录。
    try { return await check(); }
    catch (error) {
      if (policy?.action) await this.identity.rejection(policy.action, request.id, error, request.identity?.user.id);
      throw error;
    }
  }
}
