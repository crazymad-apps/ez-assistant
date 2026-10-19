import { SiteDownload, SiteDownloadQuery, SiteContent, SiteChangelogEntry, SiteNode, SiteNotesPreviewInput } from '../contracts/site-content.js';
import { releaseNotes } from '../site-content/markdown.js';
import { limitedText, object } from './manifest.js';
import { Body, Controller, Get, Inject, Param, Post, Put, Query, Req, Res } from '@nestjs/common';
import { RouteConfig } from '@nestjs/platform-fastify';
import { ApiExtraModels, ApiBody, ApiConsumes, ApiDefaultResponse, ApiNoContentResponse, ApiOkResponse, ApiOperation, ApiParam, ApiProduces, ApiTags } from '@nestjs/swagger';
import type { FastifyReply } from 'fastify';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { DesktopReleaseDetail, DesktopReleaseNotesInput, DesktopReleasePublicationInput, DesktopReleaseSummary,
  DesktopReleaseUploadResult, DesktopUpdate, DesktopUpdateQuery } from '../contracts/desktop-releases.js';
import { DesktopReleaseService } from './service.js';
import { IdentityError } from '../identity/errors.js';

@Controller('api')
@Access('admin')
@ApiTags('desktop-releases')
@ApiExtraModels(SiteContent)
@ApiDefaultResponse({ type: CenterErrorResponse, description: '权限、发布状态或文件校验失败' })
export class DesktopReleasesController {
  constructor(@Inject(DesktopReleaseService) private readonly releases: DesktopReleaseService) {}

  @Get('site/downloads')
  @Access('public')
  @ApiOperation({ operationId: 'getSiteDownloads', summary: '各平台最新可用安装包，或指定版本', security: [] })
  @ApiOkResponse({ type: [SiteDownload] })
  downloads(@Query() query: SiteDownloadQuery) { return this.releases.downloads(query); }

  @Get('site/changelog')
  @Access('public')
  @ApiOperation({ operationId: 'getSiteChangelog', summary: '读取曾发布版本的更新日志', security: [] })
  @ApiOkResponse({ type: [SiteChangelogEntry] })
  changelog() { return this.releases.changelog(); }

  @Post('desktop-releases/notes-preview')
  @RouteConfig({ desktopReleaseNotes: true })
  @ApiOperation({ operationId: 'previewDesktopReleaseNotes', summary: '预览未保存的 Markdown 更新日志' })
  @ApiBody({ type: SiteNotesPreviewInput })
  @ApiOkResponse({ type: [SiteNode] })
  previewNotes(@Body() body: unknown) {
    const input = object(body, ['notes']);
    return releaseNotes(limitedText(input.notes, 16 * 1024));
  }

  @Get('desktop-releases')
  @ApiOperation({ operationId: 'listDesktopReleases', summary: '安装版本列表' })
  @ApiOkResponse({ type: [DesktopReleaseSummary] })
  list(@Req() request: AuthenticatedRequest) { return this.releases.list(request.token); }

  @Get('desktop-releases/:version')
  @ApiOperation({ operationId: 'getDesktopRelease', summary: '安装版本详情' })
  @ApiParam({ name: 'version' })
  @ApiOkResponse({ type: DesktopReleaseDetail })
  get(@Param('version') version: string, @Req() request: AuthenticatedRequest) { return this.releases.get(version, request.token); }

  @Post('desktop-releases/packages')
  @Access('admin', 'desktop_release_uploaded')
  @RouteConfig({ desktopReleaseUpload: true })
  @ApiOperation({ operationId: 'uploadDesktopRelease', summary: '上传平台安装文件，新建草稿或覆盖同版本平台文件并保留发布状态' })
  @ApiConsumes('multipart/form-data')
  @ApiBody({ schema: { type: 'object', required: ['manifest', 'signature', 'files'], properties: {
    manifest: { type: 'string', description: '第一个字段：release.json 原始 UTF-8 字节的 base64（最多 64 KiB 解码字节）' },
    signature: { type: 'string', description: '第二个字段：release.json.sig 原文' },
    files: { type: 'array', minItems: 1, maxItems: 2, items: { type: 'string', format: 'binary' } },
  } } })
  @ApiOkResponse({ type: DesktopReleaseUploadResult })
  async upload(@Req() request: AuthenticatedRequest, @Res() reply: FastifyReply) {
    const client = new AbortController();
    const disconnect = () => { if (!reply.raw.writableFinished) client.abort(new IdentityError(499, 'DESKTOP_RELEASE_CANCELLED')); };
    const received = () => request.raw.socket.setTimeout(0);
    reply.raw.once('close', disconnect); request.raw.once('end', received);
    try { return reply.status(200).send(await this.releases.upload(request, request.token, request.id, client.signal)); }
    finally { reply.raw.off('close', disconnect); request.raw.off('end', received); }
  }

  @Put('desktop-releases/:version/notes')
  @Access('admin', 'desktop_release_notes_changed')
  @RouteConfig({ desktopReleaseNotes: true })
  @ApiOperation({ operationId: 'updateDesktopReleaseNotes', summary: '修改更新说明' })
  @ApiParam({ name: 'version' })
  @ApiBody({ type: DesktopReleaseNotesInput })
  @ApiOkResponse({ type: DesktopReleaseDetail })
  notes(@Param('version') version: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.releases.notes(version, body, request.token, request.id);
  }

  @Put('desktop-releases/:version/targets/:target/publication')
  @Access('admin', 'desktop_release_publication_changed')
  @ApiOperation({ operationId: 'setDesktopReleasePublication', summary: '发布或撤回一个平台' })
  @ApiParam({ name: 'version' })
  @ApiParam({ name: 'target' })
  @ApiBody({ type: DesktopReleasePublicationInput })
  @ApiOkResponse({ type: DesktopReleaseDetail })
  publication(@Param('version') version: string, @Param('target') target: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.releases.publish(version, target, body, request.token, request.id);
  }

  @Get('desktop-updates/:target/:current_version')
  @Access('public')
  @ApiOperation({ operationId: 'discoverDesktopUpdate', summary: '查询当前适用更新', security: [] })
  @ApiParam({ name: 'target' })
  @ApiParam({ name: 'current_version' })
  @ApiOkResponse({ type: DesktopUpdate })
  @ApiNoContentResponse({ description: '没有适用的可下载更新' })
  async discover(@Param('target') target: string, @Param('current_version') current: string, @Query() query: DesktopUpdateQuery,
    @Res() reply: FastifyReply) {
    const result = await this.releases.discover(target, current, query);
    return result ? reply.send(result) : reply.status(204).send();
  }

  @Get('desktop-updates/:target/releases/:version')
  @Access('public')
  @ApiOperation({ operationId: 'getPublishedDesktopRelease', summary: '复核指定已发布更新', security: [] })
  @ApiParam({ name: 'target' })
  @ApiParam({ name: 'version' })
  @ApiOkResponse({ type: DesktopUpdate })
  fixed(@Param('target') target: string, @Param('version') version: string) { return this.releases.fixed(target, version); }

  @Get('desktop-updates/:target/releases/:version/files/:purpose')
  @Access('public')
  @ApiOperation({ operationId: 'downloadDesktopRelease', summary: '下载已发布的安装或更新文件', security: [] })
  @ApiParam({ name: 'target' })
  @ApiParam({ name: 'version' })
  @ApiParam({ name: 'purpose', enum: ['installer', 'updater'] })
  @ApiProduces('application/octet-stream')
  @ApiOkResponse({ schema: { type: 'string', format: 'binary' } })
  async download(@Param('target') target: string, @Param('version') version: string, @Param('purpose') purpose: string,
    @Req() request: AuthenticatedRequest, @Res() reply: FastifyReply) {
    if (request.headers.range) throw new IdentityError(400, 'INVALID_REQUEST');
    const selected = await this.releases.published(target, version, purpose);
    reply.header('Content-Type', 'application/octet-stream');
    reply.header('Content-Length', selected.file!.size);
    reply.header('Content-Disposition', `attachment; filename="${selected.file!.name}"`);
    reply.raw.once('close', () => selected.stream!.destroy());
    return reply.send(selected.stream);
  }
}
