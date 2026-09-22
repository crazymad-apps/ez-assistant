import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { CatalogModel, ModelParameters, ProviderConnection } from '../contracts/models.js';

@Entity({ schema: 'public', name: 'providers', synchronize: false })
export class ProviderEntity {
  @PrimaryColumn('uuid') provider_instance_id!: string;
  @Column('jsonb') connection!: ProviderConnection;
  @Column({ type: 'text', select: false }) api_key!: string;
  @Column({ type: 'jsonb', nullable: true }) model_catalog!: CatalogModel[] | null;
  @Column({ type: 'timestamptz', nullable: true }) catalog_refreshed_at!: Date | null;
  @Column('boolean') catalog_connection_changed!: boolean;
}
@Entity({ schema: 'public', name: 'model_fixed_configs', synchronize: false })
export class FixedConfigEntity {
  @PrimaryColumn('uuid') provider_instance_id!: string;
  @PrimaryColumn('text') model_id!: string;
  @Column('text') origin!: 'online' | 'manual';
  @Column('jsonb') parameters!: ModelParameters;
  @Column('timestamptz') updated_at!: Date;
}
@Entity({ schema: 'public', name: 'model_settings', synchronize: false })
export class ModelSettingsEntity {
  @PrimaryColumn('boolean') singleton!: boolean;
  @Column({ type: 'uuid', nullable: true }) provider_instance_id!: string | null;
  @Column({ type: 'text', nullable: true }) model_id!: string | null;
}
