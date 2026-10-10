import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { FeedbackEnvironment, FeedbackKind, FeedbackSource, FeedbackStatus } from '../contracts/feedback.js';

/** 身份和目标只保存稳定引用及名称快照，删除对象不改变历史归属。 */
@Entity({ schema: 'public', name: 'feedback', synchronize: false })
export class FeedbackEntity {
  @PrimaryColumn('uuid') id!: string;
  @Column('integer') owner_user_id!: number;
  @Column('text') owner_username!: string;
  @Column('text') owner_display_name!: string;
  @Column('uuid') submission_id!: string;
  @Column('text') source!: FeedbackSource;
  @Column('text') target_kind!: FeedbackKind;
  @Column('text', { nullable: true }) target_reference!: string | null;
  @Column('text') target_name!: string;
  @Column('text', { nullable: true }) target_version!: string | null;
  @Column('text', { nullable: true }) target_content_ref!: string | null;
  @Column('text', { nullable: true }) target_location!: string | null;
  @Column('text') title!: string;
  @Column('text') description!: string;
  @Column('text', { nullable: true }) reproduction!: string | null;
  @Column('text', { nullable: true }) expected_result!: string | null;
  @Column('jsonb') environment!: FeedbackEnvironment;
  @Column('text') status!: FeedbackStatus;
  @Column('text', { nullable: true }) handling_note!: string | null;
  @Column('integer', { nullable: true }) handler_user_id!: number | null;
  @Column('text', { nullable: true }) handler_username!: string | null;
  @Column('text', { nullable: true }) handler_display_name!: string | null;
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}
