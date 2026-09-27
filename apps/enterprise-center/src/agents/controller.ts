import { Body, Controller, Delete, Get, HttpCode, Inject, Param, Post, Put, Req, Res } from '@nestjs/common';
import { RouteConfig } from '@nestjs/platform-fastify';
import { ApiBody, ApiCreatedResponse, ApiDefaultResponse, ApiHeader, ApiNoContentResponse, ApiOkResponse,
  ApiOperation, ApiParam, ApiTags } from '@nestjs/swagger';
import type { FastifyReply } from 'fastify';
import { AgentConfig, AgentDetail, AgentSummary } from '../contracts/agents.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { AgentsService } from './service.js';

@Controller('api')
@Access('admin')
@ApiTags('agents')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '输入、身份、条件写或依赖失败' })
export class AgentsController {
  constructor(@Inject(AgentsService) private readonly agents: AgentsService) {}

  @Get('agents')
  @ApiOperation({ operationId: 'listAgents', summary: '管理员读取 Agent 摘要与当前依赖诊断' })
  @ApiOkResponse({ type: [AgentSummary] })
  list(@Req() request: AuthenticatedRequest) { return this.agents.list(request.token); }

  @Post('agents')
  @RouteConfig({ agentWrite: true })
  @ApiOperation({ operationId: 'createAgent', summary: '创建完整企业 Agent 配置' })
  @ApiBody({ type: AgentConfig })
  @ApiCreatedResponse({ type: AgentConfig, headers: {
    ETag: { description: '新配置的条件写标识', schema: { type: 'string' } },
    Location: { description: '新建 Agent 的管理详情路径', schema: { type: 'string' } },
  } })
  async create(@Body() body: unknown, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.agents.create(request.token, body, request.id);
    reply.header('Location', `/api/agents/${encodeURIComponent(result.body.name)}`);
    reply.header('ETag', result.etag);
    return result.body;
  }

  @Get('agents/:name')
  @ApiOperation({ operationId: 'getAgent', summary: '管理员读取完整 Agent 与当前依赖诊断' })
  @ApiParam({ name: 'name' })
  @ApiOkResponse({ type: AgentDetail, headers: {
    ETag: { description: '当前完整配置的条件写标识', schema: { type: 'string' } },
  } })
  async get(@Param('name') name: string, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.agents.get(name, request.token);
    reply.header('ETag', result.etag);
    return result.body;
  }

  @Put('agents/:name')
  @RouteConfig({ agentWrite: true })
  @ApiOperation({ operationId: 'updateAgent', summary: '以 If-Match 修改完整 Agent 配置' })
  @ApiParam({ name: 'name' })
  @ApiHeader({ name: 'If-Match', required: true, description: '管理详情返回的当前 ETag' })
  @ApiBody({ type: AgentConfig })
  @ApiOkResponse({ type: AgentConfig, headers: {
    ETag: { description: '更新后完整配置的条件写标识', schema: { type: 'string' } },
  } })
  async update(@Param('name') name: string, @Body() body: unknown, @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply) {
    const result = await this.agents.update(name, request.token, body, request.headers['if-match'], request.id);
    reply.header('ETag', result.etag);
    return result.body;
  }

  @Delete('agents/:name')
  @HttpCode(204)
  @ApiOperation({ operationId: 'deleteAgent', summary: '以 If-Match 删除 Agent 配置' })
  @ApiParam({ name: 'name' })
  @ApiHeader({ name: 'If-Match', required: true, description: '管理详情返回的当前 ETag' })
  @ApiNoContentResponse()
  delete(@Param('name') name: string, @Req() request: AuthenticatedRequest) {
    return this.agents.delete(name, request.token, request.headers['if-match'], request.id);
  }

  @Get('runtime/agents')
  @Access('authenticated')
  @ApiOperation({ operationId: 'listRuntimeAgents', summary: '登录用户读取完整企业 Agent 配置目录' })
  @ApiOkResponse({ type: [AgentConfig] })
  runtimeList(@Req() request: AuthenticatedRequest) { return this.agents.runtimeList(request.token); }
}
