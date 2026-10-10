import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { desktopTargets } from '../desktop-releases/manifest.js';
import type { DesktopTarget, Publication } from '../desktop-releases/manifest.js';

export class DesktopReleaseFile {
  @ApiProperty({ type: String }) name!: string;
  @ApiProperty({ type: [String], enum: ['installer', 'updater'] }) purposes!: ('installer' | 'updater')[];
  @ApiProperty({ type: Number, minimum: 1, maximum: 536870912 }) size!: number;
  @ApiProperty({ type: String }) sha256!: string;
  @ApiPropertyOptional({ type: String }) signature?: string;
}
export class DesktopReleaseHost {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) min_supported_version!: string;
  @ApiProperty({ type: [Number] }) enterprise_protocols!: number[];
}
export class DesktopReleaseManifest {
  @ApiProperty({ enum: ['ez-assistant'] }) product!: 'ez-assistant';
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) min_supported_version!: string;
  @ApiProperty({ enum: desktopTargets }) target!: DesktopTarget;
  @ApiProperty({ type: DesktopReleaseHost }) host!: DesktopReleaseHost;
  @ApiProperty({ type: [DesktopReleaseFile] }) files!: DesktopReleaseFile[];
}
export class DesktopTargetSummary {
  @ApiProperty({ enum: desktopTargets }) target!: DesktopTarget;
  @ApiProperty({ enum: ['draft', 'published', 'withdrawn'] }) publication!: Publication;
}
export class DesktopTargetDetail extends DesktopTargetSummary {
  @ApiProperty({ type: String, description: '原始清单，供上传结果核对' }) manifest_text!: string;
  @ApiProperty({ type: String }) manifest_sha256!: string;
  @ApiProperty({ type: String, nullable: true }) first_published_at!: string | null;
  @ApiProperty({ type: DesktopReleaseManifest }) manifest!: DesktopReleaseManifest;
}
export class DesktopReleaseSummary {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) notes!: string;
  @ApiProperty({ type: [DesktopTargetSummary] }) targets!: DesktopTargetSummary[];
}
export class DesktopReleaseDetail {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) notes!: string;
  @ApiProperty({ type: String }) created_at!: string;
  @ApiProperty({ type: String }) updated_at!: string;
  @ApiProperty({ type: [DesktopTargetDetail] }) targets!: DesktopTargetDetail[];
}
export class DesktopReleaseUploadResult {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ enum: desktopTargets }) target!: DesktopTarget;
  @ApiProperty({ type: String }) manifest_sha256!: string;
  @ApiProperty({ enum: ['created', 'replaced', 'unchanged'] }) result!: 'created' | 'replaced' | 'unchanged';
}
export class DesktopReleaseNotesInput {
  @ApiProperty({ type: String, description: '更新说明，最多 16 KiB UTF-8' }) notes!: string;
  @ApiProperty({ type: String }) expected_notes!: string;
}
export class DesktopReleasePublicationInput {
  @ApiProperty({ type: String }) manifest_sha256!: string;
  @ApiProperty({ enum: ['draft', 'published', 'withdrawn'] }) expected_publication!: Publication;
  @ApiProperty({ enum: ['published', 'withdrawn'] }) publication!: 'published' | 'withdrawn';
}
export class DesktopUpdateQuery {
  @ApiPropertyOptional({ type: String, description: '仅连接远程 Host 时填写，三个 Host 字段必须同时提供' }) host_version?: string;
  @ApiPropertyOptional({ type: String }) host_min_supported_version?: string;
  @ApiPropertyOptional({ type: String, description: '逗号分隔的企业协议集合' }) host_protocols?: string;
}
export class DesktopUpdate {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) url!: string;
  @ApiProperty({ type: String }) signature!: string;
  @ApiProperty({ type: String }) notes!: string;
  @ApiProperty({ type: String }) pub_date!: string;
  @ApiProperty({ type: String }) manifest!: string;
  @ApiProperty({ type: String }) manifest_signature!: string;
  @ApiProperty({ type: String }) manifest_sha256!: string;
}
