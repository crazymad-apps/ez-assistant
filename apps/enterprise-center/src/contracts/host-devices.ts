import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { IdentityUser } from './identity.js';

export const deviceStatuses = ['pending', 'registered', 'rejected', 'withdrawn', 'revoked'] as const;
export type DeviceStatus = (typeof deviceStatuses)[number];
export class ApplyHostDevice {
  @ApiProperty({ format: 'uuid' }) center_id!: string;
  @ApiProperty({ format: 'uuid' }) id!: string;
  @ApiProperty({ format: 'uuid' }) installation_id!: string;
  @ApiProperty({ writeOnly: true, pattern: '^[0-9a-f]{64}$' }) secret!: string;
  @ApiProperty({ maxLength: 80 }) name!: string;
  @ApiProperty({ maxLength: 32 }) platform!: string;
  @ApiProperty({ maxLength: 64 }) host_version!: string;
}

export class HostDeviceGrant {
  @ApiProperty({ minimum: 1 }) user_id!: number;
  @ApiProperty() desktop_view!: boolean;
  @ApiProperty() desktop_control!: boolean;
}

export class HostDeviceState {
  @ApiProperty({ enum: deviceStatuses }) status!: DeviceStatus;
  @ApiProperty({ maxLength: 80 }) name!: string;
  @ApiProperty() enabled!: boolean;
  @ApiProperty({ type: [HostDeviceGrant], maxItems: 100 }) users!: HostDeviceGrant[];
}

export class HostDeviceSummary {
  @ApiPropertyOptional({ description: '管理员列表中的当前授权人数' }) user_count?: number;
  @ApiProperty({ format: 'uuid' }) id!: string;
  @ApiProperty({ format: 'uuid' }) installation_id!: string;
  @ApiProperty() name!: string;
  @ApiProperty() platform!: string;
  @ApiProperty() host_version!: string;
  @ApiProperty() applicant_user_id!: number;
  @ApiProperty({ enum: deviceStatuses }) status!: DeviceStatus;
  @ApiProperty() enabled!: boolean;
  @ApiProperty({ description: '当前已认证控制连接是否在线' }) online!: boolean;
  @ApiProperty({ format: 'date-time' }) created_at!: string;
  @ApiProperty({ format: 'date-time' }) updated_at!: string;
}

export class HostDeviceDetail extends HostDeviceSummary {
  @ApiProperty({ type: [IdentityUser] }) assigned_users!: IdentityUser[];
  @ApiProperty({ type: [HostDeviceGrant] }) users!: HostDeviceGrant[];
}

export class HostRegistrationResult {
  @ApiProperty({ format: 'uuid' }) center_id!: string;
  @ApiProperty({ type: HostDeviceSummary, nullable: true }) device!: HostDeviceSummary | null;
}

export class ManageHostDevice {
  @ApiProperty({ type: HostDeviceState }) expected!: HostDeviceState;
  @ApiProperty({ type: HostDeviceState }) desired!: HostDeviceState;
}

export class EndHostRegistration {
  @ApiProperty({ enum: ['withdraw', 'exit'] }) action!: 'withdraw' | 'exit';
}

export class HostDeviceQuery {
  @ApiPropertyOptional({ maxLength: 80 }) search?: string;
  @ApiProperty({ enum: ['registered', 'unregistered'], description: '按已登记／未登记分栏分页，未登记中待审批优先' }) status!: 'registered' | 'unregistered';
  @ApiPropertyOptional({ minimum: 1, maximum: 100, default: 20 }) limit?: number;
  @ApiPropertyOptional({ minimum: 0, maximum: 1000000, default: 0 }) offset?: number;
}

export class HostDevicePage {
  @ApiProperty({ type: [HostDeviceSummary] }) items!: HostDeviceSummary[];
  @ApiProperty() total!: number;
  @ApiProperty() limit!: number;
  @ApiProperty() offset!: number;
}
