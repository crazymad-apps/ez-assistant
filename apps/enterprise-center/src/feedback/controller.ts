import { Body, Controller, Get, HttpCode, Inject, Param, Patch, Post, Query, Req } from '@nestjs/common';
import { ApiBody, ApiDefaultResponse, ApiOkResponse, ApiOperation, ApiParam, ApiQuery, ApiTags } from '@nestjs/swagger';
import { AdminFeedbackQuery, FeedbackDetail, FeedbackPage, HandleFeedbackRequest, OwnFeedbackQuery, SubmitFeedbackRequest } from '../contracts/feedback.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { IdentityService } from '../identity/service.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { FeedbackService } from './service.js';

@Controller('api')
@Access('authenticated')
@ApiTags('feedback')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '输入、身份、提交标识冲突或处理冲突' })
export class FeedbackController {
  constructor(@Inject(FeedbackService) private readonly feedback: FeedbackService,
    @Inject(IdentityService) private readonly identity: IdentityService) {}

  @Post('feedback') @HttpCode(200) @Access('authenticated', 'feedback_created')
  @ApiOperation({ operationId: 'submitFeedback', summary: '提交反馈；同主体同次提交返回当前原记录' })
  @ApiBody({ type: SubmitFeedbackRequest }) @ApiOkResponse({ type: FeedbackDetail })
  submit(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('feedback_created', request.id,
      () => this.feedback.submit(request.token, body, request.id), request.identity?.user.id);
  }
  @Get('feedback')
  @ApiOperation({ operationId: 'listOwnFeedback', summary: '分页查询本人反馈' })
  @ApiQuery({ type: OwnFeedbackQuery }) @ApiOkResponse({ type: FeedbackPage })
  list(@Query() query: unknown, @Req() request: AuthenticatedRequest) {
    return this.feedback.list(request.token, query, false);
  }
  @Get('feedback/:id')
  @ApiOperation({ operationId: 'getOwnFeedback', summary: '读取本人反馈；不可访问与不存在均为未找到' })
  @ApiParam({ name: 'id', format: 'uuid' }) @ApiOkResponse({ type: FeedbackDetail })
  get(@Param('id') id: string, @Req() request: AuthenticatedRequest) {
    return this.feedback.get(request.token, id, false);
  }
  @Get('admin/feedback') @Access('admin')
  @ApiOperation({ operationId: 'listAdminFeedback', summary: '管理员按状态、来源、用户及目标查询反馈' })
  @ApiQuery({ type: AdminFeedbackQuery }) @ApiOkResponse({ type: FeedbackPage })
  adminList(@Query() query: unknown, @Req() request: AuthenticatedRequest) {
    return this.feedback.list(request.token, query, true);
  }
  @Get('admin/feedback/:id') @Access('admin')
  @ApiOperation({ operationId: 'getAdminFeedback', summary: '管理员读取反馈详情' })
  @ApiParam({ name: 'id', format: 'uuid' }) @ApiOkResponse({ type: FeedbackDetail })
  adminGet(@Param('id') id: string, @Req() request: AuthenticatedRequest) {
    return this.feedback.get(request.token, id, true);
  }
  @Patch('admin/feedback/:id') @Access('admin', 'feedback_handled')
  @ApiOperation({ operationId: 'handleFeedback', summary: '比较原状态和说明后处理反馈；同值不重复记账' })
  @ApiParam({ name: 'id', format: 'uuid' }) @ApiBody({ type: HandleFeedbackRequest })
  @ApiOkResponse({ type: FeedbackDetail })
  handle(@Param('id') id: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('feedback_handled', request.id,
      () => this.feedback.handle(request.token, id, body, request.id), request.identity?.user.id);
  }
}
