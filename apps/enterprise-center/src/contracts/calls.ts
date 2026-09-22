import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { ModelSelection, protocols } from './models.js';
import type { Protocol } from './models.js';
import type { Outcome } from '../llm/entity.js';
import type { SnapshotState } from '../llm/snapshots.js';

export const outcomes = ['in_progress', 'forwarded', 'rejected', 'upstream_error', 'interrupted', 'unknown'] as const;
export const snapshotStates = ['disabled', 'pending', 'complete', 'partial', 'failed', 'expired', 'missing'] as const;

export class RecordingSettings {
  @ApiProperty() enabled!: boolean;
  @ApiProperty({ minimum: 1, maximum: 3650 }) content_retention_days!: number;
  @ApiProperty({ minimum: 1, maximum: 3650 }) index_retention_days!: number;
}

export class CallSnapshot {
  @ApiProperty({ enum: snapshotStates }) state!: SnapshotState;
  @ApiProperty({ type: Number, nullable: true }) bytes!: number | null;
  @ApiProperty({ type: String, nullable: true }) sha256!: string | null;
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
}

export class SnapshotContent extends CallSnapshot {
  @ApiProperty({ type: String, nullable: true }) text!: string | null;
}

export class CallDetails {
  @ApiProperty({ format: 'uuid' }) id!: string;
  @ApiProperty({ type: Number, nullable: true }) user_id!: number | null;
  @ApiProperty({ type: String, nullable: true }) username!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_instance_id!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_name!: string | null;
  @ApiProperty({ type: String, nullable: true }) model_id!: string | null;
  @ApiProperty({ enum: ['proxy', 'admin_test'] }) kind!: 'proxy' | 'admin_test';
  @ApiProperty({ enum: protocols }) protocol!: Protocol;
  @ApiProperty({ format: 'date-time' }) started_at!: string;
  @ApiProperty({ type: String, nullable: true, format: 'date-time' }) ended_at!: string | null;
  @ApiProperty({ type: Number, nullable: true }) http_status!: number | null;
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
  @ApiProperty({ enum: outcomes }) outcome!: Outcome;
  @ApiProperty({ type: CallSnapshot }) request!: CallSnapshot;
  @ApiProperty({ type: CallSnapshot }) response!: CallSnapshot;
}

export class CallQuery {
  @ApiPropertyOptional({ format: 'uuid' }) id?: string;
  @ApiPropertyOptional({ type: Number }) user_id?: number;
  @ApiPropertyOptional({ format: 'uuid' }) provider_instance_id?: string;
  @ApiPropertyOptional() model_id?: string;
  @ApiPropertyOptional({ enum: outcomes }) outcome?: Outcome;
  @ApiPropertyOptional({ enum: ['proxy', 'admin_test'] }) kind?: 'proxy' | 'admin_test';
  @ApiPropertyOptional({ format: 'date-time' }) from?: string;
  @ApiPropertyOptional({ format: 'date-time' }) to?: string;
  @ApiPropertyOptional({ default: 50, minimum: 1, maximum: 200 }) limit?: number;
  @ApiPropertyOptional({ default: 0, minimum: 0, maximum: 1000000 }) offset?: number;
}

export class CallPage {
  @ApiProperty({ type: [CallDetails] }) items!: CallDetails[];
  @ApiProperty() total!: number;
  @ApiProperty() limit!: number;
  @ApiProperty() offset!: number;
}

export class TestModel extends ModelSelection {}

export class ModelTestResult {
  @ApiProperty({ format: 'uuid' }) call_id!: string;
  @ApiProperty({ type: Number, nullable: true }) http_status!: number | null;
  @ApiProperty() connected!: boolean;
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
}
