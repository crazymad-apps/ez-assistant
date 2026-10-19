import { Body, Controller, Get, HttpCode, Inject, Param, Patch, Post, Query, Req } from '@nestjs/common';
import { ApiBody, ApiDefaultResponse, ApiOkResponse, ApiOperation, ApiParam, ApiQuery, ApiTags } from '@nestjs/swagger';
import {
  ApplyHostDevice,
  EndHostRegistration,
  HostDeviceDetail,
  HostDevicePage,
  HostDeviceQuery,
  HostRegistrationResult,
  ManageHostDevice,
} from '../contracts/host-devices.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { IdentityService } from '../identity/service.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { HostDevicesService } from './service.js';

@Controller('api')
@Access('admin')
@ApiTags('host-devices')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '登记身份、用户权限或编辑冲突' })
export class HostDevicesController {
  constructor(
    @Inject(HostDevicesService) private readonly devices: HostDevicesService,
    @Inject(IdentityService) private readonly identity: IdentityService,
  ) {}
  @Post('host-devices')
  @HttpCode(200)
  @Access('authenticated', 'device_applied')
  @ApiOperation({ operationId: 'applyHostDevice', summary: '设备主动申请，重复登记标识返回当前结果' })
  @ApiBody({ type: ApplyHostDevice })
  @ApiOkResponse({ type: HostRegistrationResult })
  apply(@Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.identity.attempt(
      'device_applied',
      r.id,
      () => this.devices.apply(r.token, body, r.id),
      r.identity?.user.id,
    );
  }
  // public 仅跳过用户 Token Guard；Service 必须验证独立设备凭据，绝非匿名设备管理。
  @Get('host-devices/:id/registration')
  @Access('public')
  @ApiOperation({ operationId: 'getHostRegistration', summary: '设备凭据查询本登记；不存在时 device 为 null' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: HostRegistrationResult })
  registration(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.devices.registration(id, r.token);
  }

  @Post('host-devices/:id/registration')
  @HttpCode(200)
  @Access('public')
  @ApiOperation({ operationId: 'endHostRegistration', summary: '设备凭据撤回申请或注销；退出不要求用户登录' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiBody({ type: EndHostRegistration })
  @ApiOkResponse({ type: HostRegistrationResult })
  end(@Param('id') id: string, @Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.devices.end(id, r.token, body, r.id);
  }

  @Get('admin/host-devices')
  @ApiOperation({ operationId: 'listHostDevices', summary: '管理员按已登记／未登记分栏分页查询设备' })
  @ApiQuery({ type: HostDeviceQuery })
  @ApiOkResponse({ type: HostDevicePage })
  list(@Query() query: unknown, @Req() r: AuthenticatedRequest) {
    return this.devices.list(r.token, query);
  }

  @Get('admin/host-devices/:id')
  @ApiOperation({ operationId: 'getHostDevice', summary: '管理员查询登记与授权集合' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiOkResponse({ type: HostDeviceDetail })
  get(@Param('id') id: string, @Req() r: AuthenticatedRequest) {
    return this.devices.get(r.token, id);
  }

  @Patch('admin/host-devices/:id')
  @Access('admin', 'device_managed')
  @ApiOperation({ operationId: 'manageHostDevice', summary: '比较所见原值，原子审批、授权、启停或注销' })
  @ApiParam({ name: 'id', format: 'uuid' })
  @ApiBody({ type: ManageHostDevice })
  @ApiOkResponse({ type: HostDeviceDetail })
  manage(@Param('id') id: string, @Body() body: unknown, @Req() r: AuthenticatedRequest) {
    return this.identity.attempt(
      'device_managed',
      r.id,
      () => this.devices.manage(r.token, id, body, r.id),
      r.identity?.user.id,
    );
  }
}
