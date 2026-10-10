import { Column, Entity, PrimaryColumn } from 'typeorm';
import type { DesktopTarget, Publication } from './manifest.js';

@Entity({ name: 'desktop_releases', schema: 'public' })
export class DesktopReleaseEntity {
  @PrimaryColumn('text') version!: string;
  @Column('text') notes!: string;
  @Column('timestamptz') created_at!: Date;
  @Column('timestamptz') updated_at!: Date;
}

@Entity({ name: 'desktop_release_targets', schema: 'public' })
export class DesktopReleaseTargetEntity {
  @PrimaryColumn('text') version!: string;
  @PrimaryColumn('text') target!: DesktopTarget;
  @Column('text') manifest_text!: string;
  @Column('text') manifest_signature!: string;
  @Column('text') manifest_sha256!: string;
  @Column('text') publication!: Publication;
  @Column('timestamptz', { nullable: true }) first_published_at!: Date | null;
}
