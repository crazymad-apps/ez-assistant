import { Column, Entity, PrimaryGeneratedColumn } from 'typeorm';

@Entity({ schema: 'public', name: 'management_audit', synchronize: false })
export class AuditEntity {
  @PrimaryGeneratedColumn('identity', { type: 'integer', generatedIdentity: 'ALWAYS' }) id!: number;
  @Column({ type: 'timestamptz', default: () => 'CURRENT_TIMESTAMP' }) occurred_at!: Date;
  @Column({ type: 'integer', nullable: true }) actor_user_id!: number | null;
  @Column({ type: 'integer', nullable: true }) target_user_id!: number | null;
  @Column('text') action!: string;
  @Column('boolean') success!: boolean;
  @Column({ type: 'text', nullable: true }) reason_code!: string | null;
  @Column('uuid') request_id!: string;
  @Column({ type: 'jsonb', default: () => "'{}'::jsonb" }) details!: object;
}
