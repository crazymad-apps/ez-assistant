import { Body, Controller, Get, HttpCode, Inject, Post, Req } from '@nestjs/common';
import { ApiBody, ApiDefaultResponse, ApiNoContentResponse, ApiOkResponse, ApiOperation, ApiTags } from '@nestjs/swagger';
import { LoginResponse, IdentityResponse, LoginRequest, PasswordRequest } from '../contracts/identity.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { IdentityService } from './service.js';
import { fields } from '../validation.js';
import { loginInput, passwordInput } from './validation.js';

@Controller('api/auth')
@ApiTags('identity')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '400 输入无效；401 身份失败；403 来源/权限拒绝；409 状态变化；429 限流；503 服务不可用' })
export class IdentityController {
  constructor(@Inject(IdentityService) private readonly identity: IdentityService) {}

  @Post('login') @HttpCode(200) @Access('public', 'login')
  @ApiOperation({ operationId: 'login', summary: '统一登录，返回内存 Token 和关联 LLM key；无 Cookie 操作', security: [] })
  @ApiBody({ type: LoginRequest }) @ApiOkResponse({ type: LoginResponse })
  login(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('login', request.id, async () => {
      this.identity.limiter.take(request.ip);
      return this.identity.login(loginInput(body), request.id);
    });
  }

  @Get('me')
  @ApiOperation({ operationId: 'getCurrentIdentity', summary: '查询本人和中心身份，不重复返回凭据' })
  @ApiOkResponse({ type: IdentityResponse })
  me(@Req() request: AuthenticatedRequest) { return request.identity!; }

  @Post('logout') @HttpCode(204) @Access('optional', 'logout')
  @ApiOperation({ operationId: 'logout', summary: '仅废弃当前 Token 和关联 key；缺失/无效/重复退出均成功；请求体为 {}', security: [{}, { tokenBearer: [] }] })
  @ApiBody({ schema: { type: 'object', additionalProperties: false } }) @ApiNoContentResponse()
  logout(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('logout', request.id, async () => {
      fields(body, []);
      await this.identity.logout(request.token, request.id);
    });
  }

  @Post('password') @HttpCode(204) @Access('authenticated', 'self_password_changed')
  @ApiOperation({ operationId: 'changeOwnPassword', summary: '验证原密码并修改本人密码，撤销本人全部 Token；不是管理重置' })
  @ApiBody({ type: PasswordRequest }) @ApiNoContentResponse()
  password(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('self_password_changed', request.id, async () => {
      await this.identity.changePassword(request.token, passwordInput(body), request.id);
    });
  }
}
