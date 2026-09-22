import { Body, Controller, Get, Inject, Param, Post, Put, Query, Req, Res, HttpCode } from '@nestjs/common';
import { ApiBody, ApiDefaultResponse, ApiOkResponse, ApiOperation, ApiParam, ApiQuery, ApiTags } from '@nestjs/swagger';
import type { FastifyReply } from 'fastify';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { CenterErrorResponse } from '../contracts/error.js';
import {
  CallDetails,
  CallPage,
  CallQuery,
  RecordingSettings,
  SnapshotContent,
  TestModel,
  ModelTestResult,
} from '../contracts/calls.js';
import { enumeration } from '../models/parameters.js';
import { queryFields } from '../validation.js';
import { CallManagement } from './management.js';

@Controller('api')
@Access('admin')
@ApiTags('llm-calls')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '身份、参数或存储失败' })
export class CallsController {
  constructor(@Inject(CallManagement) private readonly calls: CallManagement) {}

  @Get('llm-calls')
  @ApiOperation({ operationId: 'listLlmCalls' })
  @ApiQuery({ type: CallQuery })
  @ApiOkResponse({ type: CallPage })
  list(@Query() query: unknown, @Req() request: AuthenticatedRequest) {
    return this.calls.list(request.token, query);
  }

  @Get('llm-calls/:id')
  @ApiOperation({ operationId: 'getLlmCall' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: CallDetails })
  get(@Param('id') id: string, @Req() request: AuthenticatedRequest) {
    return this.calls.get(request.token, id);
  }

  @Get('llm-calls/:id/snapshot')
  @ApiOperation({ operationId: 'getLlmSnapshot' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiQuery({ name: 'side', enum: ['request', 'response'] })
  @ApiOkResponse({ type: SnapshotContent })
  snapshot(@Param('id') id: string, @Query() value: unknown, @Req() request: AuthenticatedRequest) {
    const query = queryFields(value, ['side']);
    return this.calls.snapshot(request.token, id, enumeration(query.side, ['request', 'response']), request.id);
  }

  @Get('llm-recording-settings')
  @ApiOperation({ operationId: 'getLlmRecordingSettings' })
  @ApiOkResponse({ type: RecordingSettings })
  settings(@Req() request: AuthenticatedRequest) {
    return this.calls.settings(request.token);
  }

  @Put('llm-recording-settings')
  @ApiOperation({ operationId: 'saveLlmRecordingSettings' })
  @ApiBody({ type: RecordingSettings })
  @ApiOkResponse({ type: RecordingSettings })
  save(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.calls.saveSettings(request.token, body, request.id);
  }

  @Post('model-configuration/test')
  @HttpCode(200)
  @ApiOperation({ operationId: 'testModelConfiguration' })
  @ApiBody({ type: TestModel })
  @ApiOkResponse({ type: ModelTestResult })
  async test(
    @Body() body: unknown,
    @Req() request: AuthenticatedRequest,
    @Res({ passthrough: true }) reply: FastifyReply,
  ) {
    const cancelled = new AbortController();

    const close = () => {
      if (!reply.raw.writableFinished) cancelled.abort();
    };

    reply.raw.once('close', close);
    request.raw.socket.setTimeout?.(35000);
    try {
      return await this.calls.test(request.token, body, cancelled.signal);
    } finally {
      reply.raw.removeListener('close', close);
      request.raw.socket.setTimeout?.(10000);
    }
  }
}
