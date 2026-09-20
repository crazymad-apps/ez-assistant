import { ApiProperty } from '@nestjs/swagger';

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
  @ApiProperty({ enum: ['user', 'admin'] }) readonly role!: 'user' | 'admin';
  @ApiProperty({ type: Boolean }) readonly is_super_admin!: boolean;
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}
export class IdentityResponse {
  @ApiProperty({ type: String, format: 'uuid' }) readonly center_id!: string;
  @ApiProperty({ type: IdentityUser }) readonly user!: IdentityUser;
}
export class LoginResponse extends IdentityResponse {
  @ApiProperty({ type: String, pattern: '^ct_[a-f0-9]{64}$', description: '用于 Bearer 身份认证；仅内存保存，退出、撤销或中心重启后失效' })
  readonly token!: string;
  @ApiProperty({ type: String, pattern: '^cl_[a-f0-9]{64}$', description: '仅供后续 LLM 代理使用，随本次 token 一起撤销；不用于身份 API' })
  readonly llm_key!: string;
}
