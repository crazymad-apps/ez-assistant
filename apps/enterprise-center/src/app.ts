import { registerSitePages } from './site-pages.js';
import { registerSiteContent } from './site-content/static.js';
import { enterpriseProtocol } from './protocol.js';
import 'reflect-metadata';
import { readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { Controller, Get, HttpException, Inject, Module } from '@nestjs/common';
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
import { Access, HttpSecurity, requestCredential } from './security.js';
import { UsersController } from './users/controller.js';
import { UsersService } from './users/service.js';
import { AuditController } from './audit/controller.js';
import { AuditService } from './audit/service.js';
import { ModelsController } from './models/controller.js';
import { ModelsService } from './models/service.js';
import { ModelTemplates } from './models/templates.js';
import { ModelProxy } from './llm/proxy.js';
import { CallRecords } from './llm/records.js';
import { SnapshotFiles } from './llm/snapshots.js';
import { CallManagement } from './llm/management.js';
import { CallsController } from './llm/controller.js';
import { PluginFiles } from './plugins/files.js';
import { PluginService } from './plugins/service.js';
import { PluginsController } from './plugins/controller.js';
import { PluginError } from './plugins/errors.js';
import { AgentsController } from './agents/controller.js';
import { AgentsService } from './agents/service.js';
import { AgentError } from './agents/errors.js';
import { HostDevicesController } from './host-devices/controller.js';
import { HostNetwork } from './host-devices/network.js';
import { HostDevicesService } from './host-devices/service.js';
import { FeedbackController } from './feedback/controller.js';
import { FeedbackService } from './feedback/service.js';
import type { CenterConfig } from './config.js';
import fastifyStatic from '@fastify/static';
import multipart from '@fastify/multipart';
import { DesktopReleaseFiles } from './desktop-releases/files.js';
import { DesktopReleaseService } from './desktop-releases/service.js';
import { DesktopReleasesController } from './desktop-releases/controller.js';

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
  constructor(@Inject(IdentityService) private readonly identity: IdentityService) {}
  @Get('info')
  @Access('public')
  @ApiOperation({ operationId: 'getCenterInfo', summary: '查询中心软件与协议兼容信息', security: [] })
  @ApiOkResponse({ type: CenterInfo })
  @ApiDefaultResponse({ type: CenterErrorResponse, description: '请求失败，返回脱敏原因码与请求标识' })
  async info(): Promise<CenterInfo> {
    return {
      center_id: await this.identity.centerId(),
      software_version: version,
      protocol_version: enterpriseProtocol.current,
      min_protocol_version: enterpriseProtocol.minimum,
      // M1 市场分发和 M2 完整 Agent 配置读取均已通过真实 HTTP 验证。
      capabilities: ['managed_models', 'llm_proxy', 'plugin_market', 'managed_agents', 'feedback', 'host_registration'],
    };
  }
}

@Module({
  controllers: [
    InfoController,
    IdentityController,
    UsersController,
    AuditController,
    ModelsController,
    CallsController,
    PluginsController,
    AgentsController,
    FeedbackController,
    HostDevicesController,
    DesktopReleasesController,
  ],
})
class CenterModule {}

class SafeHttpErrors implements ExceptionFilter {
  catch(error: unknown, context: ArgumentsHost): void {
    const http = context.switchToHttp();
    const request = http.getRequest<FastifyRequest>();
    const reply = http.getResponse<FastifyReply>();
    let status = 503;
    if (error instanceof IdentityError) status = error.status;
    else if (error instanceof PluginError) status = error.status;
    else if (error instanceof AgentError) status = error.status;
    else if (error instanceof HttpException) status = error.getStatus();
    const payload =
      error instanceof IdentityError || error instanceof PluginError || error instanceof AgentError
        ? { error: { code: error.code, message: error.message, request_id: request.id } }
        : httpFailure(status, request.id);
    void reply.status(status).send(payload);
  }
}

/** 只装配 HTTP；存储准入由外层 owner 完成后才调用 listen，避免建半初始化服务。 */
export async function createHttpApp(
  identity?: { source: DataSource; origin: string } & Pick<
    CenterConfig,
    'siteContentRoot' | 'desktopReleaseRoot' | 'desktopUpdateKeyFile' | 'templatesFile' | 'snapshotRoot' | 'pluginRoot' | 'llmConnectTimeout' | 'llmIdleTimeout' | 'llmWebsocket'
  >,
  adminRoot?: string,
  siteRoot?: string,
): Promise<NestFastifyApplication> {
  const adapter = new FastifyAdapter({
    logger: false,
    bodyLimit: 16 * 1024,
    requestIdHeader: false,

    genReqId: () => randomUUID(),

    trustProxy: false,
    requestTimeout: 30 * 60 * 1000,
    connectionTimeout: 10000,
    forceCloseConnections: true,
  });
  // 离线契约导出不初始化 DataSource；无已准入依赖时明确失败，不提供 mock 成功路径。
  const identities = new IdentityService(identity?.source);
  const templates = new ModelTemplates(identity?.templatesFile);
  if (identity) await templates.initialize();
  const models = new ModelsService(identities, identity?.source, templates, identity?.origin);
  const records = new CallRecords(identity?.source, new SnapshotFiles(identity?.snapshotRoot, adminRoot));
  const proxy = new ModelProxy(identities, models, records, {
    connectTimeout: identity?.llmConnectTimeout,
    idleTimeout: identity?.llmIdleTimeout,
    websocket: identity?.llmWebsocket,
  });
  const pluginFiles = new PluginFiles(identity?.pluginRoot, adminRoot);
  const releaseFiles = new DesktopReleaseFiles(identity?.desktopReleaseRoot, adminRoot);
  const releases = new DesktopReleaseService(identities, identity?.source, releaseFiles, identity?.origin, identity?.desktopUpdateKeyFile);
  const devices = new HostDevicesService(identities);
  const network = new HostNetwork(devices, identities);
  const app = await NestFactory.create<NestFastifyApplication>(
    {
      module: CenterModule,
      providers: [
        { provide: DesktopReleaseService, useValue: releases },
        { provide: ModelProxy, useValue: proxy },
        { provide: CallManagement, useValue: new CallManagement(identities, identity?.source, proxy, models) },
        {
          provide: ModelsService,
          useValue: models,
        },
        { provide: IdentityService, useValue: identities },
        { provide: HostDevicesService, useValue: devices },
        { provide: HostNetwork, useValue: network },
        { provide: FeedbackService, useValue: new FeedbackService(identities, identity?.source) },
        { provide: UsersService, useValue: new UsersService(identities, identity?.source) },
        { provide: AuditService, useValue: new AuditService(identity?.source) },
        { provide: PluginService, useValue: new PluginService(identities, identity?.source, pluginFiles) },
        { provide: AgentsService, useValue: new AgentsService(identities, identity?.source) },
      ],
    },
    adapter,
    { logger: false, abortOnError: false },
  );
  // Center 仅接受显式 Bearer；任意来源可访问，预检无需登录，不启用跨域 Cookie。
  app.enableCors({
    origin: '*',
    methods: ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'],
    credentials: false,
    exposedHeaders: ['ETag', 'Content-Disposition', 'X-EZ-Call-Id', 'X-EZ-Center-Control', 'X-Request-Id', 'Retry-After'],
  });
  app.useGlobalFilters(new SafeHttpErrors());
  app.useGlobalGuards(new HttpSecurity(app.get(IdentityService)));
  adapter.getInstance().addHook('onSend', async (request, reply, payload) => {
    if (!request.url.startsWith('/site-content/') || !reply.hasHeader('Cache-Control'))
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
    if (identity) await records.initialize();
    proxy.install(adapter.getInstance());
    network.install(adapter.getInstance());
    const fastify = adapter.getInstance();
    await fastify.register(multipart, { limits: { fields: 2, files: 2, parts: 4, fileSize: 512 * 1024 * 1024, fieldSize: 96 * 1024 } });
    // 原样交给上传 owner 逐块接收；通用 JSON bodyLimit 不适用于完整归档。
    fastify.addContentTypeParser(['application/zip', 'application/gzip'], (_request, body, done) => {
      done(null, body);
    });
    fastify.addHook('onRoute', options => {
      const config = options.config as { pluginUpload?: boolean; agentWrite?: boolean;
        pluginResolve?: boolean; desktopReleaseUpload?: boolean; desktopReleaseNotes?: boolean } | undefined;
      if (config?.desktopReleaseUpload) options.bodyLimit = 1024 * 1024 * 1024 + 128 * 1024;
      else if (config?.desktopReleaseNotes) options.bodyLimit = 256 * 1024;
      else if (config?.pluginUpload)
        options.bodyLimit = 2 * 1024 * 1024 * 1024;
      else if (config?.agentWrite) options.bodyLimit = 2 * 1024 * 1024;
      else if (config?.pluginResolve) options.bodyLimit = 64 * 1024;
    });
    const bodyTimers = new WeakMap<FastifyRequest, NodeJS.Timeout>();
    fastify.addHook('onRequest', async (request, reply) => {
      const config = request.routeOptions.config as { pluginUpload?: boolean; desktopReleaseUpload?: boolean };
      const desktopUpload = config?.desktopReleaseUpload === true;
      const upload = config?.pluginUpload === true || desktopUpload;
      if (!upload) {
        // 底层接收上限因大包提高后，普通管理 JSON 仍保持原有短超时。
        if (['POST', 'PUT', 'PATCH'].includes(request.method)) {
          const timer = setTimeout(() => request.raw.destroy(), 10000);
          bodyTimers.set(request, timer);
          request.raw.once('close', () => clearTimeout(timer));
        }
        if (/^(application\/zip|application\/gzip)$/i.test(request.headers['content-type'] ?? ''))
          return reply.status(400).send(httpFailure(400, request.id));
        return;
      }
      try {
        // 在读取大包正文前先鉴权，拒绝请求不占用包暂存与扫描资源。
        const token = requestCredential(request, !desktopUpload, desktopUpload);
        if (request.headers['content-encoding'] && request.headers['content-encoding'] !== 'identity')
          throw new IdentityError(400, 'INVALID_REQUEST');
        const user = (await identities.me(token)).user;
        if (!user.is_super_admin && user.role !== 'admin') throw new IdentityError(403, 'ADMIN_REQUIRED');
        request.raw.socket.setTimeout(60000);
      } catch (error) {
        const failure = error instanceof IdentityError ? error : new IdentityError(503, 'SERVICE_UNAVAILABLE');
        if (failure.status < 500) await identities.rejection(desktopUpload ? 'desktop_release_uploaded' : 'plugin_package_uploaded', request.id, failure);
        return reply.status(failure.status).send({ error: { code: failure.code, message: failure.message, request_id: request.id } });
      }
    });
    fastify.addHook('preValidation', async request => {
      const timer = bodyTimers.get(request);
      if (timer) clearTimeout(timer);
    });
    fastify.addHook('onResponse', async request => {
      const timer = bodyTimers.get(request);
      if (timer) clearTimeout(timer);
      if ((request.routeOptions.config as { pluginUpload?: boolean; desktopReleaseUpload?: boolean } | undefined)?.pluginUpload
        || (request.routeOptions.config as { desktopReleaseUpload?: boolean } | undefined)?.desktopReleaseUpload)
        request.raw.socket.setTimeout(10000);
    });
    registerSiteContent(fastify, identity?.siteContentRoot);
    if (siteRoot) registerSitePages(fastify, siteRoot);
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
    if (identity?.pluginRoot) await pluginFiles.initialize();
    if (identity?.desktopReleaseRoot) await releaseFiles.initialize();
    await releases.initialize();
    // Fastify 解析错误也由 Nest 的异常层转入全局过滤器，不重复覆盖同 scope 的 handler。
    await adapter.getInstance().ready();
    return app;
  } catch (error) {
    await proxy.close();
    await pluginFiles.close();
    await releaseFiles.close();
    await app.close();
    throw error;
  }
}
