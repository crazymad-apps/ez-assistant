import { Body, Controller, Get, HttpCode, Inject, Param, Patch, Post, Query, Req } from '@nestjs/common';
import { ApiBody, ApiCreatedResponse, ApiDefaultResponse, ApiNoContentResponse, ApiOkResponse, ApiOperation, ApiParam, ApiTags } from '@nestjs/swagger';
import { CreateUserRequest, ResetPasswordRequest, UpdateUserRequest, UserPage, UserQuery, UserRecord } from '../contracts/users.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { IdentityService } from '../identity/service.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { recordId } from '../validation.js';
import { UsersService } from './service.js';
import { createUserInput, resetPasswordInput, updateUserInput, userQuery } from './validation.js';

@Controller('api/users') @Access('admin') @ApiTags('users')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '400 无效输入；401 登录失效；403 无管理权限；404 用户不存在；409 账号冲突/管理员保护；429 限流；503 服务不可用' })
export class UsersController {
  constructor(@Inject(UsersService) private readonly users: UsersService, @Inject(IdentityService) private readonly identity: IdentityService) {}

  @Get() @Access('admin', 'users_read')
  @ApiOperation({ operationId: 'listUsers', summary: '管理员查询用户，按创建时间/ID 升序分页' })
  @ApiOkResponse({ type: UserPage })
  list(@Query() query: UserQuery, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('users_read', request.id, () => this.users.list(userQuery(query)), request.identity!.user.id);
  }

  @Post() @Access('admin', 'user_created')
  @ApiOperation({ operationId: 'createUser', summary: '创建普通用户或管理员，不允许传入超级管理员标记' })
  @ApiBody({ type: CreateUserRequest }) @ApiCreatedResponse({ type: UserRecord })
  create(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('user_created', request.id, () => this.users.create(request.token, createUserInput(body), request.id), request.identity!.user.id);
  }

  @Patch(':id') @Access('admin', 'user_updated')
  @ApiOperation({ operationId: 'updateUser', summary: '编辑显示名称、角色或启用状态，至少一项；不允许改账号/超级管理员标记', description: '带超级管理员标记的用户禁止停用或降权；角色/启用状态实际改变后撤销目标全部 Token。相同值不重复写入或审计。' })
  @ApiParam({ name: 'id', schema: { type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 } })
  @ApiBody({ type: UpdateUserRequest }) @ApiOkResponse({ type: UserRecord })
  update(@Param('id') id: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('user_updated', request.id, () => this.users.update(request.token, recordId(id), updateUserInput(body), request.id), request.identity!.user.id);
  }

  @Post(':id/reset-password') @HttpCode(204) @Access('admin', 'user_password_reset')
  @ApiOperation({ operationId: 'resetUserPassword', summary: '管理员为任意用户重置密码（包括自己和超级管理员），不校验原密码；撤销目标全部 Token' })
  @ApiParam({ name: 'id', schema: { type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 } })
  @ApiBody({ type: ResetPasswordRequest }) @ApiNoContentResponse()
  reset(@Param('id') id: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('user_password_reset', request.id,
      () => this.users.resetPassword(request.token, recordId(id), resetPasswordInput(body), request.id), request.identity!.user.id);
  }
}
