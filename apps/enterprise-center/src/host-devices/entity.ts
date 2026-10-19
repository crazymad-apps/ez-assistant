import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { DeviceStatus } from '../contracts/host-devices.js';

/** 身份摘要不进入通用列表投影；申请人只是稳定历史引用。 */
@Entity({ schema: 'public', name: 'host_devices', synchronize: false })
export class HostDeviceEntity {
  @PrimaryColumn('uuid') id!: string;
  @Column('uuid') installation_id!: string;
  @Column('text', { select: false }) credential_hash!: string;
  @Column('text') name!: string;
  @Column('text') platform!: string;
  @Column('text') host_version!: string;
  @Column('integer') applicant_user_id!: number;
  @Column('text') status!: DeviceStatus;
  @Column('boolean') enabled!: boolean;
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}
@Entity({ schema: 'public', name: 'host_device_users', synchronize: false })
export class HostDeviceUserEntity {
  @PrimaryColumn('uuid') device_id!: string;
  @PrimaryColumn('integer') user_id!: number;
  @Column('boolean') desktop_view!: boolean;
  @Column('boolean') desktop_control!: boolean;
}
