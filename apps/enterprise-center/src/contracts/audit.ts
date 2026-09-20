import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { PageQuery } from './users.js';

export class AuditQuery extends PageQuery {
  @ApiPropertyOptional({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647, description: '按审计记录 ID 精确查询，供详情路由直接加载' }) readonly id?: number;
  @ApiPropertyOptional({ type: String, format: 'date-time', description: 'UTC ISO 8601 起始时间（含），须以 Z 结尾' }) readonly from?: string;
  @ApiPropertyOptional({ type: String, format: 'date-time', description: 'UTC ISO 8601 结束时间（不含），须以 Z 结尾' }) readonly to?: string;
  @ApiPropertyOptional({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 }) readonly actor_user_id?: number;
  @ApiPropertyOptional({ type: String, pattern: '^[a-z_]{1,64}$' }) readonly action?: string;
  @ApiPropertyOptional({ type: Boolean }) readonly success?: boolean;
}
export class AuditUserFields {
  @ApiPropertyOptional({ type: String }) readonly display_name?: string;
  @ApiPropertyOptional({ enum: ['user', 'admin'] }) readonly role?: 'user' | 'admin';
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
}
export class AuditDetails {
  @ApiPropertyOptional({ type: AuditUserFields }) readonly before?: AuditUserFields;
  @ApiPropertyOptional({ type: AuditUserFields }) readonly after?: AuditUserFields;
}
export class AuditRecord {
  @ApiProperty({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 }) readonly id!: number;
  @ApiProperty({ type: String, format: 'date-time' }) readonly occurred_at!: string;
  @ApiProperty({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647, nullable: true }) readonly actor_user_id!: number | null;
  @ApiProperty({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647, nullable: true }) readonly target_user_id!: number | null;
  @ApiProperty({ type: String, nullable: true, description: '关联操作者的登录用户名；未认证或关联用户不存在时为空' }) readonly actor_username!: string | null;
  @ApiProperty({ type: String, nullable: true, description: '关联目标用户的登录用户名；无目标或关联用户不存在时为空' }) readonly target_username!: string | null;
  @ApiProperty({ type: String }) readonly action!: string;
  @ApiProperty({ type: Boolean }) readonly success!: boolean;
  @ApiProperty({ type: String, nullable: true }) readonly reason_code!: string | null;
  @ApiProperty({ type: String, format: 'uuid' }) readonly request_id!: string;
  @ApiProperty({ type: AuditDetails }) readonly details!: AuditDetails;
}
export class AuditPage {
  @ApiProperty({ type: [AuditRecord] }) readonly items!: AuditRecord[];
  @ApiProperty({ type: Number, minimum: 0 }) readonly total!: number;
  @ApiProperty({ type: Number }) readonly limit!: number;
  @ApiProperty({ type: Number }) readonly offset!: number;
}
