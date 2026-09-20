import 'reflect-metadata';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { Controller, Get, HttpException, Module } from '@nestjs/common';
import type { ArgumentsHost, ExceptionFilter } from '@nestjs/common';
import { NestFactory } from '@nestjs/core';
import { FastifyAdapter } from '@nestjs/platform-fastify';
import type { NestFastifyApplication } from '@nestjs/platform-fastify';
import type { FastifyReply, FastifyRequest } from 'fastify';
import type { DataSource } from 'typeorm';
import { ApiDefaultResponse, ApiOkResponse, ApiOperation, ApiTags, SwaggerModule } from '@nestjs/swagger';
import { CenterInfo } from './contracts/info.js';
import { CenterErrorResponse } from './contracts/error.js';
import { createOpenApiDocument } from './openapi.js';
import { safeError } from './errors.js';
import { IdentityController } from './identity/controller.js';
import { IdentityService } from './identity/service.js';
import { IdentityError } from './identity/errors.js';
import { Access, HttpSecurity } from './security.js';
import { UsersController } from './users/controller.js';
import { UsersService } from './users/service.js';
import { AuditController } from './audit/controller.js';
import { AuditService } from './audit/service.js';
import fastifyStatic from '@fastify/static';

const version = (
  JSON.parse(readFileSync(new URL('./package-info.json', import.meta.url), 'utf8')) as { version: string }
).version;

function httpFailure(status: number, requestId: string): CenterErrorResponse {
  const failure =
    status === 404
      ? { code: 'NOT_FOUND', message: '接口不存在。' }
      : { code: 'INVALID_REQUEST', message: '请求格式或大小不符合要求。' };
  return { error: { ...(status >= 500 ? safeError(undefined) : failure), request_id: requestId } };
}

@Controller('api')
@ApiTags('center')
class InfoController {
  @Get('info')
  @Access('public')
  @ApiOperation({ operationId: 'getCenterInfo', summary: '查询中心软件与协议兼容信息', security: [] })
  @ApiOkResponse({ type: CenterInfo })
  @ApiDefaultResponse({ type: CenterErrorResponse, description: '请求失败，返回脱敏原因码与请求标识' })
  info(): CenterInfo {
    return { software_version: version, protocol_version: 1, min_protocol_version: 1 };
  }
}

@Module({ controllers: [InfoController, IdentityController, UsersController, AuditController] })
class CenterModule {}

class SafeHttpErrors implements ExceptionFilter {
  catch(error: unknown, context: ArgumentsHost): void {
    const http = context.switchToHttp();
    const request = http.getRequest<FastifyRequest>();
    const reply = http.getResponse<FastifyReply>();
    let status = 503;
    if (error instanceof IdentityError) status = error.status;
    else if (error instanceof HttpException) status = error.getStatus();
    const payload =
      error instanceof IdentityError
        ? { error: { code: error.code, message: error.message, request_id: request.id } }
        : httpFailure(status, request.id);
    void reply.status(status).send(payload);
  }
}

/** 只装配 HTTP；存储准入由外层 owner 完成后才调用 listen，避免建半初始化服务。 */
export async function createHttpApp(
  identity?: { source: DataSource; origin: string },
  adminRoot?: string,
): Promise<NestFastifyApplication> {
  const adapter = new FastifyAdapter({
    logger: false,
    bodyLimit: 16 * 1024,
    requestIdHeader: false,
    genReqId: () => randomUUID(),
    trustProxy: false,
    requestTimeout: 10000,
    connectionTimeout: 10000,
    forceCloseConnections: true,
  });
  // 离线契约导出不初始化 DataSource；无已准入依赖时明确失败，不提供 mock 成功路径。
  const identities = new IdentityService(identity?.source);
  const app = await NestFactory.create<NestFastifyApplication>(
    {
      module: CenterModule,
      providers: [
        { provide: IdentityService, useValue: identities },
        { provide: UsersService, useValue: new UsersService(identities, identity?.source) },
        { provide: AuditService, useValue: new AuditService(identity?.source) },
      ],
    },
    adapter,
    { logger: false, abortOnError: false },
  );
  app.useGlobalFilters(new SafeHttpErrors());
  app.useGlobalGuards(new HttpSecurity(app.get(IdentityService), identity?.origin));
  adapter.getInstance().addHook('onSend', async (request, reply, payload) => {
    reply.header('Cache-Control', 'no-store');
    reply.header('X-Content-Type-Options', 'nosniff');
    reply.header('Referrer-Policy', 'no-referrer');
    if (request.url.split('?')[0]?.startsWith('/admin')) {
      // Ant Design 的运行时样式需要 inline style；脚本仍仅允许同源文件，不允许 eval/inline script。
      reply.header(
        'Content-Security-Policy',
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
      );
    }
    return payload;
  });
  try {
    if (adminRoot) {
      adapter.getInstance().register(fastifyStatic, {
        root: adminRoot,
        prefix: '/admin',
        decorateReply: false,
        index: 'index.html',
        dotfiles: 'deny',
        list: false,
        redirect: true,
        // 不暴露构建清单、源码、配置或 SPA 兜底；未知页面与 API 继续返回真实 404。
        allowedPath: (path) => path === '/index.html' || path === '/' || /^\/assets\/[a-zA-Z0-9_./-]+$/.test(path),
      });
    }
    // Swagger 由 Fastify 直接注册，明确公开；业务接口统一经过 Nest 全局鉴权。
    SwaggerModule.setup('api/docs', app, () => createOpenApiDocument(app), {
      jsonDocumentUrl: '/api/openapi.json',
      raw: ['json'],
      swaggerOptions: { validatorUrl: null, persistAuthorization: false },
    });
    await app.init();
    // Fastify 解析错误也由 Nest 的异常层转入全局过滤器，不重复覆盖同 scope 的 handler。
    await adapter.getInstance().ready();
    return app;
  } catch (error) {
    await app.close();
    throw error;
  }
}
