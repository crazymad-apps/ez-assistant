import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';

export const feedbackKinds = ['platform', 'plugin', 'knowledge'] as const;
export const feedbackStatuses = ['pending', 'processing', 'resolved', 'closed'] as const;
export const feedbackSources = ['manual', 'agent'] as const;
export type FeedbackKind = typeof feedbackKinds[number];
export type FeedbackStatus = typeof feedbackStatuses[number];
export type FeedbackSource = typeof feedbackSources[number];

export class FeedbackTarget {
  @ApiProperty({ enum: feedbackKinds }) kind!: FeedbackKind;
  @ApiProperty({ type: String, nullable: true, maxLength: 512 }) reference!: string | null;
  @ApiProperty({ maxLength: 120 }) name!: string;
  @ApiProperty({ type: String, nullable: true, maxLength: 64 }) version!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 512 }) content_ref!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 512 }) location!: string | null;
}
export class FeedbackContent {
  @ApiProperty({ type: FeedbackTarget }) target!: FeedbackTarget;
  @ApiProperty({ maxLength: 120 }) title!: string;
  @ApiProperty({ maxLength: 4000 }) description!: string;
  @ApiProperty({ type: String, nullable: true, maxLength: 2000 }) reproduction!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 2000 }) expected_result!: string | null;
}
export class FeedbackEnvironment {
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) host_version!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) host_os!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) host_arch!: string | null;
  @ApiProperty({ enum: ['desktop', 'web', 'agent'] }) client_kind!: 'desktop' | 'web' | 'agent';
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) client_version!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) client_os!: string | null;
  @ApiProperty({ type: String, nullable: true, maxLength: 128 }) client_arch!: string | null;
}
export class SubmitFeedbackRequest extends FeedbackContent {
  @ApiProperty({ format: 'uuid' }) submission_id!: string;
  @ApiProperty({ enum: feedbackSources }) source!: FeedbackSource;
  @ApiProperty({ type: FeedbackEnvironment }) environment!: FeedbackEnvironment;
}
export class FeedbackSummary {
  @ApiProperty({ format: 'uuid' }) id!: string;
  @ApiProperty() title!: string;
  @ApiProperty({ enum: feedbackKinds }) target_kind!: FeedbackKind;
  @ApiProperty() target_name!: string;
  @ApiProperty({ enum: feedbackStatuses }) status!: FeedbackStatus;
  @ApiProperty({ enum: feedbackSources }) source!: FeedbackSource;
  @ApiProperty() owner_user_id!: number;
  @ApiProperty() owner_username!: string;
  @ApiProperty() owner_display_name!: string;
  @ApiProperty({ format: 'date-time' }) created_at!: string;
  @ApiProperty({ format: 'date-time' }) updated_at!: string;
}
export class FeedbackDetail extends FeedbackSummary {
  @ApiProperty({ format: 'uuid' }) submission_id!: string;
  @ApiProperty({ type: FeedbackTarget }) target!: FeedbackTarget;
  @ApiProperty() description!: string;
  @ApiProperty({ type: String, nullable: true }) reproduction!: string | null;
  @ApiProperty({ type: String, nullable: true }) expected_result!: string | null;
  @ApiProperty({ type: FeedbackEnvironment }) environment!: FeedbackEnvironment;
  @ApiProperty({ type: String, nullable: true }) handling_note!: string | null;
  @ApiProperty({ type: Number, nullable: true }) handler_user_id!: number | null;
  @ApiProperty({ type: String, nullable: true }) handler_username!: string | null;
  @ApiProperty({ type: String, nullable: true }) handler_display_name!: string | null;
}
export class HandleFeedbackRequest {
  @ApiProperty({ enum: feedbackStatuses }) expected_status!: FeedbackStatus;
  @ApiProperty({ type: String, nullable: true, maxLength: 2000 }) expected_handling_note!: string | null;
  @ApiProperty({ enum: feedbackStatuses }) status!: FeedbackStatus;
  @ApiProperty({ type: String, nullable: true, maxLength: 2000 }) handling_note!: string | null;
}
export class FeedbackQuery {
  @ApiPropertyOptional({ minimum: 1, maximum: 20, default: 20 }) limit?: number;
  @ApiPropertyOptional({ minimum: 0, maximum: 1000000, default: 0 }) offset?: number;
  @ApiPropertyOptional({ enum: feedbackStatuses }) status?: FeedbackStatus;
  @ApiPropertyOptional({ enum: feedbackKinds }) target_kind?: FeedbackKind;
  @ApiPropertyOptional({ maxLength: 512, description: '须同时指定目标类型，精确匹配稳定引用' }) target_reference?: string;
  @ApiPropertyOptional({ description: '仅知识目标；true 筛选无稳定引用，false 筛选有引用' }) target_unresolved?: boolean;
}
export class OwnFeedbackQuery extends FeedbackQuery {
  @ApiPropertyOptional({ format: 'uuid' }) submission_id?: string;
}
export class AdminFeedbackQuery extends FeedbackQuery {
  @ApiPropertyOptional({ enum: feedbackSources }) source?: FeedbackSource;
  @ApiPropertyOptional({ minimum: 1, maximum: 2147483647 }) owner_user_id?: number;
  @ApiPropertyOptional({ maxLength: 120, description: '标题字面包含，不解释通配符' }) title?: string;
}
export class FeedbackPage {
  @ApiProperty({ type: [FeedbackSummary] }) items!: FeedbackSummary[];
  @ApiProperty() total!: number;
  @ApiProperty() limit!: number;
  @ApiProperty() offset!: number;
}
