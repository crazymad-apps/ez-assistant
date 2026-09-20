import { Controller, Get, Inject, Query, Req } from '@nestjs/common';
import { ApiDefaultResponse, ApiOkResponse, ApiOperation, ApiTags } from '@nestjs/swagger';
import { AuditPage, AuditQuery } from '../contracts/audit.js';
import { CenterErrorResponse } from '../contracts/error.js';
import { IdentityService } from '../identity/service.js';
import { Access } from '../security.js';
import type { AuthenticatedRequest } from '../security.js';
import { AuditService, auditQuery } from './service.js';

@Controller('api/audit') @Access('admin', 'audit_read') @ApiTags('audit')
@ApiDefaultResponse({ type: CenterErrorResponse, description: '400 无效查询；401 登录失效；403 无管理权限；503 服务不可用' })
export class AuditController {
  constructor(@Inject(AuditService) private readonly audit: AuditService, @Inject(IdentityService) private readonly identity: IdentityService) {}
  @Get()
  @ApiOperation({ operationId: 'listManagementAudit', summary: '管理员只读审计查询，按时间/ID 倒序分页，不返回秘密' })
  @ApiOkResponse({ type: AuditPage })
  list(@Query() query: AuditQuery, @Req() request: AuthenticatedRequest) {
    return this.identity.attempt('audit_read', request.id, () => this.audit.list(auditQuery(query)), request.identity!.user.id);
  }
}
