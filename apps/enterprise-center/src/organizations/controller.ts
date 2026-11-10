import { Body, Controller, Delete, Get, HttpCode, Inject, Param, Patch, Post, Put, Query, Req } from '@nestjs/common';
import { ApiBody, ApiCreatedResponse, ApiDefaultResponse, ApiNoContentResponse, ApiOkResponse, ApiOperation, ApiTags } from '@nestjs/swagger';
import { CreateOrganization, MemberCandidate, MemberCandidateQuery, MemberPage, MemberQuery, Organization, SaveMember, UpdateMember, UpdateOrganization } from '../contracts/organizations.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { fields, invalid, page, queryBoolean, queryFields, recordId, uuid } from '../validation.js';
import { username } from '../identity/validation.js';
import { OrganizationsService } from './service.js';

function organizationFields(input: Record<string, unknown>) {
  const { code, name } = input;
  if (typeof code !== 'string' || !/^[a-z0-9][a-z0-9_-]{0,63}$/.test(code) ||
    typeof name !== 'string' || !name.trim() || [...name.trim()].length > 64 || /[\p{Cc}]/u.test(name)) invalid();
  return { code, name: name.trim() };
}

function memberRole(role: unknown): SaveMember['role'] {
  if (role !== 'admin' && role !== 'member') invalid();
  return role;
}

function memberFields(body: unknown): SaveMember {
  const { role, enabled } = fields(body, ['role', 'enabled']);
  if (typeof enabled !== 'boolean') invalid();
  return { role: memberRole(role), enabled };
}

@ApiTags('organizations')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '400 请求无效；401 登录失效；403 范围拒绝；409 状态冲突' })
@Access('system')
@Controller('api/organizations')
export class OrganizationsController {
  constructor(@Inject(OrganizationsService) private readonly organizations: OrganizationsService) {}

  @Get()
  @ApiOperation({ operationId: 'listOrganizations' })
  @ApiOkResponse({ type: [Organization] })
  list(@Req() request: AuthenticatedRequest) {
    return this.organizations.list(request.token);
  }

  @Post()
  @ApiOperation({ operationId: 'createOrganization' })
  @ApiBody({ type: CreateOrganization })
  @ApiCreatedResponse({ type: Organization })
  create(@Body() body: unknown, @Req() request: AuthenticatedRequest) {
    const input = fields(body, ['code', 'name', 'administrator_user_id']);
    if (typeof input.administrator_user_id !== 'number') invalid();
    return this.organizations.create(request.token, {
      ...organizationFields(input), administrator_user_id: recordId(String(input.administrator_user_id)),
    }, request.id);
  }

  @Patch(':id')
  @ApiOperation({ operationId: 'updateOrganization' })
  @ApiBody({ type: UpdateOrganization })
  @ApiOkResponse({ type: Organization })
  update(@Param('id') id: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    const input = fields(body, ['code', 'name', 'enabled']);
    if (typeof input.enabled !== 'boolean') invalid();
    return this.organizations.update(request.token, uuid(id),
      { ...organizationFields(input), enabled: input.enabled }, request.id);
  }

  @Patch(':id/members/:userId')
  @ApiOperation({ operationId: 'updateSystemOrganizationMember' })
  @ApiBody({ type: UpdateMember })
  @ApiOkResponse({ type: SaveMember })
  updateMember(@Param('id') id: string, @Param('userId') userId: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    const { role, enabled } = fields(body, [], ['role', 'enabled']);
    if ((role === undefined && enabled === undefined) || (enabled !== undefined && typeof enabled !== 'boolean')) invalid();
    return this.organizations.updateSystemMember(request.token, uuid(id), recordId(userId),
      { role: role === undefined ? undefined : memberRole(role), enabled }, request.id);
  }

  @Post(':id/members/:userId')
  @ApiOperation({ operationId: 'addSystemOrganizationMember' })
  @ApiBody({ type: SaveMember })
  @ApiCreatedResponse({ type: SaveMember })
  addMember(@Param('id') id: string, @Param('userId') userId: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    return this.organizations.addSystemMember(request.token, uuid(id), recordId(userId), memberFields(body), request.id);
  }

  @Delete(':id/members/:userId')
  @HttpCode(204)
  @ApiOperation({ operationId: 'removeSystemOrganizationMember' })
  @ApiNoContentResponse()
  async removeMember(@Param('id') id: string, @Param('userId') userId: string, @Req() request: AuthenticatedRequest) {
    await this.organizations.removeSystemMember(request.token, uuid(id), recordId(userId), request.id);
  }
}

@ApiTags('organizations')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '400 请求无效；401 登录失效；403 范围拒绝；409 状态冲突' })
@Access('organization')
@Controller('api/organization-members')
export class MembersController {
  constructor(@Inject(OrganizationsService) private readonly organizations: OrganizationsService) {}

  @Get()
  @ApiOperation({ operationId: 'listOrganizationMembers' })
  @ApiOkResponse({ type: MemberPage })
  list(@Query() query: MemberQuery, @Req() request: AuthenticatedRequest) {
    const input = queryFields(query, ['search', 'role', 'enabled', 'active_only', 'limit', 'offset']);
    if (input.search && [...input.search].length > 64) invalid();
    return this.organizations.members(request.token, { ...page(input), search: input.search?.trim(),
      active_only: input.active_only === undefined ? undefined : queryBoolean(input.active_only),
      role: input.role === undefined ? undefined : memberRole(input.role),
      enabled: input.enabled === undefined ? undefined : queryBoolean(input.enabled) });
  }

  @Get('candidate')
  @ApiOperation({ operationId: 'findOrganizationMemberCandidate' })
  @ApiOkResponse({ type: MemberCandidate })
  candidate(@Query() query: MemberCandidateQuery, @Req() request: AuthenticatedRequest) {
    return this.organizations.candidate(request.token, username(queryFields(query, ['username']).username));
  }

  @Put(':id')
  @HttpCode(204)
  @ApiOperation({ operationId: 'saveOrganizationMember' })
  @ApiBody({ type: SaveMember })
  @ApiNoContentResponse()
  async save(@Param('id') id: string, @Body() body: unknown, @Req() request: AuthenticatedRequest) {
    await this.organizations.changeMember(request.token, recordId(id), memberFields(body), request.id);
  }

  @Delete(':id')
  @HttpCode(204)
  @ApiOperation({ operationId: 'removeOrganizationMember' })
  @ApiNoContentResponse()
  async remove(@Param('id') id: string, @Req() request: AuthenticatedRequest) {
    await this.organizations.changeMember(request.token, recordId(id), undefined, request.id);
  }
}
