import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';

export class LoginRequest {
  @ApiProperty({ type: String, description: '3—64 位 ASCII 账号，trim 后转小写', maxLength: 128 })
  readonly username!: string;
  @ApiProperty({ type: String, minLength: 1, maxLength: 128, writeOnly: true, format: 'password' })
  readonly password!: string;
}
export class PasswordRequest {
  @ApiProperty({ type: String, minLength: 1, maxLength: 128, writeOnly: true, format: 'password' })
  readonly old_password!: string;
  @ApiProperty({ type: String, minLength: 6, maxLength: 128, pattern: '^(?=[\\s\\S]*[a-zA-Z])(?=[\\s\\S]*[0-9])[\\s\\S]*$', writeOnly: true, format: 'password', description: '至少 6 位，包含英文字母和数字；不裁剪空白，最多 512 UTF-8 字节' })
  readonly new_password!: string;
}
export class IdentityUser {
  @ApiProperty({ type: 'integer', format: 'int32', minimum: 1, maximum: 2147483647 }) readonly id!: number;
  @ApiProperty({ type: String }) readonly username!: string;
  @ApiProperty({ type: String }) readonly display_name!: string;
  @ApiProperty({ enum: ['user', 'system_admin'] }) readonly role!: 'user' | 'system_admin';
  @ApiProperty({ type: Boolean }) readonly is_super_admin!: boolean;
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}
export class AccessScope {
  @ApiProperty({ enum: ['unselected', 'system', 'organization'] })
  readonly kind!: 'unselected' | 'system' | 'organization';

  @ApiPropertyOptional({ type: String, format: 'uuid', description: '仅 organization 范围携带' })
  readonly organization_id?: string;
}

export class ScopeView extends AccessScope {
  @ApiPropertyOptional({ type: String }) readonly name?: string;
  @ApiPropertyOptional({ type: String }) readonly code?: string;
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
  @ApiPropertyOptional({ enum: ['member', 'admin'] }) readonly member_role?: 'member' | 'admin';
  @ApiProperty({ type: Boolean }) readonly can_manage!: boolean;
}

export class ScopeQuery {
  @ApiProperty({ enum: ['admin', 'client'] }) readonly entry!: 'admin' | 'client';
}

export class ScopeSelection extends AccessScope {
  @ApiProperty({ enum: ['admin', 'client'] }) readonly entry!: 'admin' | 'client';
}

export class IdentityResponse {
  @ApiProperty({ type: String, format: 'uuid' }) readonly center_id!: string;
  @ApiProperty({ type: String, format: 'uuid' }) readonly legacy_organization_id!: string;
  @ApiProperty({ type: IdentityUser }) readonly user!: IdentityUser;
  @ApiProperty({ type: ScopeView }) readonly scope!: ScopeView;
  @ApiProperty({ type: String, format: 'date-time' }) readonly expires_at!: string;
}

export class LoginResponse extends IdentityResponse {
  @ApiProperty({ type: String, description: '有效期固定 7 天的 JWT。撤销或中心重启后立即失效。' })
  readonly token!: string;

  @ApiPropertyOptional({ type: String, pattern: '^cl_[a-f0-9]{64}$', description: '仅有效组织成员的客户端范围签发，随 JWT 撤销' })
  readonly llm_key?: string;
}
