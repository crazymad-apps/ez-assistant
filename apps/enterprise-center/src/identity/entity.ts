import { Column, Entity, PrimaryColumn } from 'typeorm';

@Entity({ schema: 'public', name: 'center_identity', synchronize: false })
export class CenterIdentityEntity {
  @PrimaryColumn({ type: 'boolean', default: true }) singleton!: boolean;
  @Column('uuid') center_id!: string;
  @Column('uuid') legacy_organization_id!: string;
  @Column({ type: 'text', nullable: true }) public_origin!: string | null;
  @Column({ type: 'timestamptz', default: () => 'CURRENT_TIMESTAMP' }) created_at!: Date;
}
