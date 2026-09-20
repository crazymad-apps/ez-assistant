import { Column, Entity, PrimaryGeneratedColumn } from 'typeorm';

/** 仅映射现有表；DDL/约束由升级 SQL 管理，不能把此实体直接作为 API 响应。 */
@Entity({ schema: 'public', name: 'users', synchronize: false })
export class UserEntity {
  @PrimaryGeneratedColumn('identity', { type: 'integer', generatedIdentity: 'ALWAYS' }) id!: number;
  @Column({ type: 'varchar', length: 64 }) username!: string;
  @Column({ type: 'varchar', length: 64 }) display_name!: string;
  @Column('text') role!: 'admin' | 'user';
  @Column({ type: 'boolean', default: false }) is_super_admin!: boolean;
  @Column({ type: 'boolean', default: true }) enabled!: boolean;
  @Column({ type: 'text', select: false }) password_hash!: string;
  @Column({ type: 'timestamptz', default: () => 'CURRENT_TIMESTAMP' }) created_at!: Date;
  @Column({ type: 'timestamptz', default: () => 'CURRENT_TIMESTAMP' }) updated_at!: Date;
}
