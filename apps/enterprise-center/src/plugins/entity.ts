import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { PluginManifest } from './manifest.js';

/** 展示与发布状态归插件本体所有，不随某个版本包替换而重置。 */
@Entity({ schema: 'public', name: 'plugins', synchronize: false })
export class PluginEntity {
  @PrimaryColumn('text') name!: string;
  @Column('text') display_name!: string;
  @Column('text') description!: string;
  @Column('text') publication!: 'unpublished' | 'listed' | 'deprecated';
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}

/** 每个名称与版本只映射当前完整包；旧 SHA 仅作为文件保留，不形成数据库修订历史。 */
@Entity({ schema: 'public', name: 'plugin_packages', synchronize: false })
export class PluginPackageEntity {
  @PrimaryColumn('text') plugin_name!: string;
  @PrimaryColumn('text') version!: string;
  @Column('jsonb') manifest!: PluginManifest;
  @Column('text') archive_format!: 'zip' | 'tar_gzip';
  @Column('text') sha256!: string;
  @Column('bigint') size_bytes!: string;
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}
