import { Body, Controller, Get, HttpCode, Inject, Param, Post, Put, Req, Res } from '@nestjs/common';
import { RouteConfig } from '@nestjs/platform-fastify';
import { ApiBody, ApiConsumes, ApiCreatedResponse, ApiDefaultResponse, ApiHeader, ApiOkResponse,
  ApiOperation, ApiParam, ApiProduces, ApiTags } from '@nestjs/swagger';
import type { FastifyReply } from 'fastify';
import type { Readable } from 'node:stream';
import { CenterErrorResponse } from '../contracts/error.js';
import { PackageCurrent, PackageUploadResult, PluginDescriptionInput, PluginDetail,
  PluginPublicationInput, PluginRecord, PluginResolution, PluginResolveInput } from '../contracts/plugins.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { PluginService } from './service.js';
import { PluginError } from './errors.js';
import { IdentityError } from '../identity/errors.js';

@Controller('api')
@Access('admin')
@ApiTags('plugins')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '身份、归档、存储或提交失败' })
export class PluginsController {
  constructor(@Inject(PluginService) private readonly plugins: PluginService) {}

  @Get('plugins')
  @ApiOperation({ operationId: 'listPlugins', summary: '管理员读取全部插件及当前版本' })
  @ApiOkResponse({ type: [PluginRecord] })
  list(@Req() request: AuthenticatedRequest) { return this.plugins.list(request.token); }

  @Get('plugins/:name')
  @ApiOperation({ operationId: 'getPlugin', summary: '管理员读取插件详情与条件写 ETag' })
  @ApiParam({ name: 'name' })
  @ApiOkResponse({ type: PluginDetail, headers: {
    ETag: { description: '当前展示与上架事实的条件写标识', schema: { type: 'string' } },
  } })
  async get(@Param('name') name: string, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.plugins.get(name, request.token);
    reply.header('ETag', result.etag!);
    return result.body;
  }

  @Put('plugins/:name')
  @ApiOperation({ operationId: 'updatePlugin', summary: '以 If-Match 修改插件说明' })
  @ApiParam({ name: 'name' })
  @ApiHeader({ name: 'If-Match', required: true, description: '管理详情返回的当前 ETag' })
  @ApiBody({ type: PluginDescriptionInput })
  @ApiOkResponse({ type: PluginDetail, headers: {
    ETag: { description: '更新后的条件写标识', schema: { type: 'string' } },
  } })
  async edit(@Param('name') name: string, @Body() body: unknown, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.plugins.edit(name, request.token, body, request.headers['if-match'], request.id);
    reply.header('ETag', result.etag);
    return result.body;
  }

  @Put('plugins/:name/publication')
  @ApiOperation({ operationId: 'setPluginPublication', summary: '以 If-Match 上架或下架插件' })
  @ApiParam({ name: 'name' })
  @ApiHeader({ name: 'If-Match', required: true, description: '管理详情返回的当前 ETag' })
  @ApiBody({ type: PluginPublicationInput })
  @ApiOkResponse({ type: PluginDetail, headers: {
    ETag: { description: '更新后的条件写标识', schema: { type: 'string' } },
  } })
  async publication(@Param('name') name: string, @Body() body: unknown, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.plugins.setPublication(name, request.token, body,
      request.headers['if-match'], request.id);
    reply.header('ETag', result.etag);
    return result.body;
  }

  @Post('plugin-packages')
  @RouteConfig({ pluginUpload: true })
  @ApiOperation({ operationId: 'uploadPluginPackage', summary: '上传或替换一个完整插件版本包' })
  @ApiConsumes('application/zip', 'application/gzip')
  @ApiBody({ schema: { type: 'string', format: 'binary' } })
  @ApiCreatedResponse({ type: PackageUploadResult })
  @ApiOkResponse({ type: PackageUploadResult })
  async upload(@Body() body: Readable, @Req() request: AuthenticatedRequest, @Res() reply: FastifyReply) {
    if (!body || typeof body[Symbol.asyncIterator] !== 'function') throw new IdentityError(400, 'INVALID_REQUEST');
    const format = request.headers['content-type'] === 'application/zip' ? 'zip' : 'tar_gzip';
    const client = new AbortController();
    // 连接断开只取消尚未提交的工作；若提交已完成但回包丢失，调用方须查询当前 SHA。
    const disconnect = () => { if (!reply.raw.writableFinished)
      client.abort(new PluginError(499, 'PACKAGE_CANCELLED')); };
    const received = () => request.raw.socket.setTimeout(0);
    reply.raw.once('close', disconnect);
    request.raw.once('end', received);
    try {
      const result = await this.plugins.upload(body, format, request.token, request.id,
        request.identity!.user.id, client.signal);
      return reply.status(result.result === 'created' ? 201 : 200).send(result);
    } finally {
      reply.raw.off('close', disconnect);
      request.raw.off('end', received);
    }
  }

  @Get('plugin-packages/:name/versions/:version')
  @ApiOperation({ operationId: 'getCurrentPluginPackage', summary: '核对某版本当前完整包' })
  @ApiParam({ name: 'name' })
  @ApiParam({ name: 'version' })
  @ApiOkResponse({ type: PackageCurrent })
  current(@Param('name') name: string, @Param('version') version: string, @Req() request: AuthenticatedRequest) {
    return this.plugins.current(name, version, request.token);
  }

  @Get('runtime/plugins')
  @Access('authenticated')
  @ApiOperation({ operationId: 'listRuntimePlugins', summary: '读取在架插件目录' })
  @ApiOkResponse({ type: [PluginRecord] })
  runtimeList(@Req() request: AuthenticatedRequest) { return this.plugins.list(request.token, true); }

  @Get('runtime/plugins/:name')
  @Access('authenticated')
  @ApiOperation({ operationId: 'getRuntimePlugin', summary: '读取插件发布状态与在架版本' })
  @ApiParam({ name: 'name' })
  @ApiOkResponse({ type: PluginRecord })
  async runtimeGet(@Param('name') name: string, @Req() request: AuthenticatedRequest) {
    return (await this.plugins.get(name, request.token, true)).body;
  }

  @Post('runtime/plugins/:name/resolve')
  @HttpCode(200)
  @Access('authenticated')
  @RouteConfig({ pluginResolve: true })
  @ApiOperation({ operationId: 'resolveRuntimePlugin', summary: '按平台、Host 和能力选择当前完整包' })
  @ApiParam({ name: 'name' })
  @ApiBody({ type: PluginResolveInput })
  @ApiOkResponse({ type: PluginResolution })
  resolve(@Param('name') name: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.plugins.resolve(name, request.token, body);
  }

  @Get('runtime/plugins/:name/versions/:version/package')
  @Access('authenticated')
  @ApiOperation({ operationId: 'downloadRuntimePluginPackage', summary: '带当前 SHA 条件下载完整原包' })
  @ApiParam({ name: 'name' })
  @ApiParam({ name: 'version' })
  @ApiHeader({ name: 'If-Match', required: true, description: '版本当前 SHA 的带引号 ETag' })
  @ApiProduces('application/zip', 'application/gzip')
  @ApiOkResponse({ schema: { type: 'string', format: 'binary' }, headers: {
    'Content-Length': { description: '原包字节数', schema: { type: 'integer' } },
    ETag: { description: '当前原包 SHA 的条件标识', schema: { type: 'string' } },
    'X-Plugin-SHA256': { description: '原包 SHA-256', schema: { type: 'string' } },
  } })
  async download(@Param('name') name: string, @Param('version') version: string,
    @Req() request: AuthenticatedRequest, @Res() reply: FastifyReply) {
    if (request.headers.range) throw new IdentityError(400, 'INVALID_REQUEST');
    const selected = await this.plugins.download(name, version, request.token, request.headers['if-match']);
    const sha = selected.package.sha256;
    reply.header('Content-Type', selected.package.archive_format === 'zip' ? 'application/zip' : 'application/gzip');
    reply.header('Content-Length', selected.package.size_bytes);
    reply.header('ETag', `"${sha}"`);
    reply.header('X-Plugin-SHA256', sha);
    reply.header('Content-Disposition', `attachment; filename="${name}-${version}.${selected.package.archive_format === 'zip' ? 'zip' : 'tar.gz'}"`);
    reply.raw.once('close', () => selected.stream.destroy());
    return reply.send(selected.stream);
  }
}
