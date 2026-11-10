import { Body, Controller, Get, HttpCode, Inject, Post, Query, Req } from '@nestjs/common';
import {
  ApiBody,
  ApiDefaultResponse,
  ApiNoContentResponse,
  ApiOkResponse,
  ApiOperation,
  ApiTags,
} from '@nestjs/swagger';
import { LoginResponse, IdentityResponse, LoginRequest, PasswordRequest, ScopeQuery, ScopeSelection, ScopeView } from '../contracts/identity.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { IdentityService } from './service.js';
import { fields, invalid, queryFields } from '../validation.js';
import { parseScope } from './token.js';
import { loginInput, passwordInput } from './validation.js';

@Controller('api/auth')
@ApiTags('identity')
@ApiDefaultResponse({
  type: CenterErrorResponse,
  description: '400 输入无效；401 身份失败；403 权限拒绝；409 状态变化；429 限流；503 服务不可用',
})
export class IdentityController {
  constructor(@Inject(IdentityService) private readonly identity: IdentityService) {}

  @Post('login')
  @HttpCode(200)
  @Access('public', 'login')
  @ApiOperation({
    operationId: 'login',
    summary: '认证账号并签发未选择范围的 JWT；无 Cookie 操作',
    security: [],
  })
  @ApiBody({ type: LoginRequest })
  @ApiOkResponse({ type: LoginResponse })
  login(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('login', request.id, async () => {
      this.identity.limiter.take(request.ip);
      return this.identity.login(loginInput(body), request.id);
    });
  }

  @Get('me')
  @Access('account')
  @ApiOperation({ operationId: 'getCurrentIdentity', summary: '查询本人和中心身份，不重复返回凭据' })
  @ApiOkResponse({ type: IdentityResponse })
  me(@Req() request: AuthenticatedRequest) {
    return request.identity!;
  }

  @Get('scopes')
  @Access('account')
  @ApiOperation({ operationId: 'listAccessScopes', summary: '按后台或客户端入口列出当前账号可选范围' })
  @ApiOkResponse({ type: [ScopeView] })
  scopes(@Query() query: ScopeQuery, @Req() request: AuthenticatedRequest) {
    const { entry } = queryFields(query, ['entry']);
    if (entry !== 'admin' && entry !== 'client') invalid();
    return this.identity.scopes(request.token, entry);
  }

  @Post('scope')
  @HttpCode(200)
  @Access('account', 'scope_exchanged')
  @ApiOperation({ operationId: 'exchangeAccessScope', summary: '显式选择范围，提交后撤销原 JWT；同范围不续期' })
  @ApiBody({ type: ScopeSelection })
  @ApiOkResponse({ type: LoginResponse })
  exchange(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    const { entry, ...scope } = fields(body, ['entry', 'kind'], ['organization_id']);
    if (entry !== 'admin' && entry !== 'client') invalid();
    return this.identity.attempt('scope_exchanged', request.id,
      () => this.identity.exchange(request.token, parseScope(scope), entry, request.id), request.identity!.user.id);
  }

  @Post('logout')
  @HttpCode(204)
  @Access('optional', 'logout')
  @ApiOperation({
    operationId: 'logout',
    summary: '仅废弃当前 Token 和关联 key；缺失/无效/重复退出均成功；请求体为 {}',
    security: [{}, { tokenBearer: [] }],
  })
  @ApiBody({ schema: { type: 'object', additionalProperties: false } })
  @ApiNoContentResponse()
  logout(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('logout', request.id, async () => {
      fields(body, []);
      await this.identity.logout(request.token, request.id);
    });
  }

  @Post('password')
  @HttpCode(204)
  @Access('account', 'self_password_changed')
  @ApiOperation({
    operationId: 'changeOwnPassword',
    summary: '验证原密码并修改本人密码，保留已有 Token/key；不是管理重置',
  })
  @ApiBody({ type: PasswordRequest })
  @ApiNoContentResponse()
  password(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('self_password_changed', request.id, async () => {
      await this.identity.changePassword(request.token, passwordInput(body), request.id);
    });
  }
}
