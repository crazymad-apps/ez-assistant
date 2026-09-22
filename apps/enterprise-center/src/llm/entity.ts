import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { Protocol } from '../contracts/models.js';
import type { SnapshotState } from './snapshots.js';

export type Outcome = 'in_progress' | 'forwarded' | 'rejected' | 'upstream_error' | 'interrupted' | 'unknown';

@Entity({ schema: 'public', name: 'llm_recording_settings', synchronize: false })
export class RecordingEntity {
  @PrimaryColumn('boolean') singleton!: boolean;
  @Column('boolean') enabled!: boolean;
  @Column('integer') content_retention_days!: number;
  @Column('integer') index_retention_days!: number;
}

@Entity({ schema: 'public', name: 'llm_calls', synchronize: false })
export class CallEntity {
  @PrimaryColumn('uuid') id!: string;
  @Column({ type: 'integer', nullable: true }) user_id!: number | null;
  @Column({ type: 'text', nullable: true }) username!: string | null;
  @Column({ type: 'uuid', nullable: true }) provider_instance_id!: string | null;
  @Column({ type: 'text', nullable: true }) provider_name!: string | null;
  @Column({ type: 'text', nullable: true }) model_id!: string | null;
  @Column('text') kind!: 'proxy' | 'admin_test';
  @Column('text') protocol!: Protocol;
  @Column('timestamptz') started_at!: Date;
  @Column({ type: 'timestamptz', nullable: true }) ended_at!: Date | null;
  @Column({ type: 'integer', nullable: true }) http_status!: number | null;
  @Column({ type: 'text', nullable: true }) reason!: string | null;
  @Column('text') outcome!: Outcome;
  @Column('text') request_snapshot_state!: SnapshotState;
  @Column({ type: 'text', nullable: true }) request_path!: string | null;
  @Column({ type: 'bigint', nullable: true }) request_bytes!: string | null;
  @Column({ type: 'text', nullable: true }) request_sha256!: string | null;
  @Column({ type: 'text', nullable: true }) request_snapshot_reason!: string | null;
  @Column('text') response_snapshot_state!: SnapshotState;
  @Column({ type: 'text', nullable: true }) response_path!: string | null;
  @Column({ type: 'bigint', nullable: true }) response_bytes!: string | null;
  @Column({ type: 'text', nullable: true }) response_sha256!: string | null;
  @Column({ type: 'text', nullable: true }) response_snapshot_reason!: string | null;
}
