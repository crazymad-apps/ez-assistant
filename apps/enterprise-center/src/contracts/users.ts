import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { IdentityUser } from './identity.js';

export class CreateUserRequest {
  @ApiProperty({ type: String, maxLength: 128, description: 'trim 并转小写后 3—64 位 ASCII 字母/数字及 ._-' }) readonly username!: string;
  @ApiProperty({ type: String, minLength: 1, maxLength: 64 }) readonly display_name!: string;
  @ApiProperty({ enum: ['user', 'admin'] }) readonly role!: 'user' | 'admin';
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
  @ApiProperty({ type: String, minLength: 6, maxLength: 128, pattern: '^(?=[\\s\\S]*[a-zA-Z])(?=[\\s\\S]*[0-9])[\\s\\S]*$', writeOnly: true, format: 'password', description: '至少 6 位，包含英文字母和数字；最多 512 UTF-8 字节' }) readonly password!: string;
}
export class UpdateUserRequest {
  @ApiPropertyOptional({ type: String, minLength: 1, maxLength: 64 }) readonly display_name?: string;
  @ApiPropertyOptional({ enum: ['user', 'admin'] }) readonly role?: 'user' | 'admin';
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
}
export class ResetPasswordRequest {
  @ApiProperty({ type: String, minLength: 6, maxLength: 128, pattern: '^(?=[\\s\\S]*[a-zA-Z])(?=[\\s\\S]*[0-9])[\\s\\S]*$', writeOnly: true, format: 'password', description: '至少 6 位，包含英文字母和数字；最多 512 UTF-8 字节；不收原密码，目标可为自己或超级管理员' }) readonly new_password!: string;
}
export class UserRecord extends IdentityUser {
  @ApiProperty({ type: String, format: 'date-time' }) readonly created_at!: string;
  @ApiProperty({ type: String, format: 'date-time' }) readonly updated_at!: string;
}
export class PageQuery {
  @ApiPropertyOptional({ type: Number, minimum: 1, maximum: 100, default: 20 }) readonly limit?: number;
  @ApiPropertyOptional({ type: Number, minimum: 0, maximum: 1000000, default: 0 }) readonly offset?: number;
}
export class UserQuery extends PageQuery {
  @ApiPropertyOptional({ type: String, maxLength: 64, description: '账号或显示名称的字面子串，不解释 SQL 通配符' }) readonly search?: string;
  @ApiPropertyOptional({ enum: ['user', 'admin'] }) readonly role?: 'user' | 'admin';
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
}
export class UserPage {
  @ApiProperty({ type: [UserRecord] }) readonly items!: UserRecord[];
  @ApiProperty({ type: Number, minimum: 0 }) readonly total!: number;
  @ApiProperty({ type: Number }) readonly limit!: number;
  @ApiProperty({ type: Number }) readonly offset!: number;
}
