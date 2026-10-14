import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { ModelSelection, protocols } from './models.js';
import type { Protocol } from './models.js';
import type { Outcome, RecordingMode, ResponseSource } from '../llm/entity.js';
import type { SnapshotState } from '../llm/snapshots.js';
import type { UsageState } from '../llm/entity.js';

export const outcomes = ['in_progress', 'forwarded', 'rejected', 'upstream_error', 'interrupted', 'unknown'] as const;
export const snapshotStates = [
  'disabled',
  'pending',
  'complete',
  'partial',
  'failed',
  'expired',
  'missing',
  'not_received',
] as const;
export const recordingModes = ['off', 'anomalies', 'all'] as const;
export const observations = ['pending', 'normal', 'anomalous', 'unconfirmed'] as const;
export const usageStates = ['pending', 'available', 'partial', 'unavailable'] as const;

export class RecordingSettings {
  @ApiProperty({ enum: recordingModes }) mode!: RecordingMode;
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
  @ApiProperty({ type: String, nullable: true }) body_base64!: string | null;
}

export class CallDetails {
  @ApiProperty({ format: 'uuid' }) id!: string;
  @ApiProperty({ type: Number, nullable: true }) user_id!: number | null;
  @ApiProperty({ type: String, nullable: true }) username!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_instance_id!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_name!: string | null;
  @ApiProperty({ type: String, nullable: true }) model_id!: string | null;
  @ApiProperty({ enum: ['proxy', 'admin_test', 'context_compaction', 'input_tokens'] }) kind!:
    'proxy' | 'admin_test' | 'context_compaction' | 'input_tokens';
  @ApiProperty({ enum: protocols }) protocol!: Protocol;
  @ApiProperty({ format: 'date-time' }) started_at!: string;
  @ApiProperty({ type: String, nullable: true, format: 'date-time' }) ended_at!: string | null;
  @ApiProperty({ type: Number, nullable: true }) http_status!: number | null;
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
  @ApiProperty({ enum: outcomes }) outcome!: Outcome;
  @ApiProperty({ enum: observations, nullable: true }) response_observation!:
    import('../llm/response-observer.js').Observation | null;
  @ApiProperty({ type: String, nullable: true }) response_observation_reason!: string | null;
  @ApiProperty({ enum: ['upstream_http', 'websocket_bridge', 'websocket_compact'], nullable: true })
  response_source!: ResponseSource | null;
  @ApiProperty({ type: String, nullable: true }) response_content_type!: string | null;
  @ApiProperty({ type: String, nullable: true }) response_content_encoding!: string | null;
  @ApiProperty({ type: CallSnapshot, nullable: true }) upstream_failure!: CallSnapshot | null;
  @ApiProperty({ enum: usageStates }) usage_state!: UsageState;
  @ApiProperty({ type: String, nullable: true }) usage_reason!: string | null;
  @ApiProperty({ type: String, nullable: true }) input_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) output_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) total_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) cached_input_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) reasoning_tokens!: string | null;
  @ApiProperty({ type: CallSnapshot }) request!: CallSnapshot;
  @ApiProperty({ type: CallSnapshot }) response!: CallSnapshot;
}

export class CallQuery {
  @ApiPropertyOptional({ format: 'uuid' }) id?: string;
  @ApiPropertyOptional({ type: Number }) user_id?: number;
  @ApiPropertyOptional({ format: 'uuid' }) provider_instance_id?: string;
  @ApiPropertyOptional() model_id?: string;
  @ApiPropertyOptional({ enum: outcomes }) outcome?: Outcome;
  @ApiPropertyOptional({ enum: observations }) response_observation?: import('../llm/response-observer.js').Observation;
  @ApiPropertyOptional({ enum: ['proxy', 'admin_test', 'context_compaction', 'input_tokens'] }) kind?:
    'proxy' | 'admin_test' | 'context_compaction' | 'input_tokens';
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

export class UsageQuery {
  @ApiPropertyOptional({ format: 'date-time' }) from?: string;
  @ApiPropertyOptional({ format: 'date-time' }) to?: string;
  @ApiPropertyOptional({ type: Number }) user_id?: number;
  @ApiPropertyOptional({ format: 'uuid' }) provider_instance_id?: string;
  @ApiPropertyOptional() model_id?: string;
  @ApiPropertyOptional({ enum: ['proxy', 'admin_test', 'context_compaction'] }) kind?:
    'proxy' | 'admin_test' | 'context_compaction';
  @ApiPropertyOptional({ enum: outcomes }) outcome?: Outcome;
  @ApiPropertyOptional({ enum: ['hour', 'day', 'month'] }) granularity?: 'hour' | 'day' | 'month';
  @ApiPropertyOptional({ enum: ['user', 'model'] }) group_by?: 'user' | 'model';
  @ApiPropertyOptional({ type: Number, default: 20 }) limit?: number;
  @ApiPropertyOptional({ type: Number, default: 0 }) offset?: number;
}

export class UsageTotals {
  @ApiProperty() calls!: string;
  @ApiProperty() forwarded!: string;
  @ApiProperty() rejected!: string;
  @ApiProperty() upstream_error!: string;
  @ApiProperty() interrupted!: string;
  @ApiProperty() unknown!: string;
  @ApiProperty() in_progress!: string;
  @ApiProperty({ type: String, nullable: true }) avg_duration_ms!: string | null;
  @ApiProperty() available!: string;
  @ApiProperty() partial!: string;
  @ApiProperty() unavailable!: string;
  @ApiProperty() pending!: string;
  @ApiProperty({ type: String, nullable: true }) input_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) output_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) total_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) cached_input_tokens!: string | null;
  @ApiProperty({ type: String, nullable: true }) reasoning_tokens!: string | null;
  @ApiProperty() input_samples!: string;
  @ApiProperty() output_samples!: string;
  @ApiProperty() total_samples!: string;
  @ApiProperty() cached_samples!: string;
  @ApiProperty() reasoning_samples!: string;
}

export class UsageBucket extends UsageTotals {
  @ApiProperty({ format: 'date-time' }) start!: string;
  @ApiProperty() incomplete!: boolean;
}

export class UsageGroup extends UsageTotals {
  @ApiProperty({ type: Number, nullable: true }) user_id!: number | null;
  @ApiProperty({ type: String, nullable: true }) username!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_instance_id!: string | null;
  @ApiProperty({ type: String, nullable: true }) provider_name!: string | null;
  @ApiProperty({ type: String, nullable: true }) model_id!: string | null;
}

export class UsagePage {
  @ApiProperty({ format: 'date-time' }) from!: string;
  @ApiProperty({ format: 'date-time' }) to!: string;
  @ApiProperty({ format: 'date-time' }) available_from!: string;
  @ApiProperty() timezone!: string;
  @ApiProperty({ enum: ['hour', 'day', 'month'] }) granularity!: 'hour' | 'day' | 'month';
  @ApiProperty({ enum: ['user', 'model'] }) group_by!: 'user' | 'model';
  @ApiProperty({ type: UsageTotals }) summary!: UsageTotals;
  @ApiProperty({ type: [UsageBucket] }) buckets!: UsageBucket[];
  @ApiProperty({ type: [UsageGroup] }) groups!: UsageGroup[];
  @ApiProperty() group_total!: number;
  @ApiProperty() limit!: number;
  @ApiProperty() offset!: number;
}

export class UsageModelOption {
  @ApiProperty({ format: 'uuid' }) provider_instance_id!: string;
  @ApiProperty() provider_name!: string;
  @ApiProperty() model_id!: string;
}

export class TestModel extends ModelSelection {}

export class ModelTestResult {
  @ApiProperty({ format: 'uuid' }) call_id!: string;
  @ApiProperty({ type: Number, nullable: true }) http_status!: number | null;
  @ApiProperty() connected!: boolean;
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
}
