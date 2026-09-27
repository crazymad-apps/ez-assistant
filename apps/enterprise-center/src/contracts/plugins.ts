import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';

export class PluginPlatform {
  @ApiProperty({ enum: ['macos', 'windows', 'linux'] }) os!: 'macos' | 'windows' | 'linux';
  @ApiProperty({ enum: ['x64', 'arm64'] }) arch!: 'x64' | 'arm64';
}

export class PackageUploadResult {
  @ApiProperty() name!: string;
  @ApiProperty() version!: string;
  @ApiProperty({ type: [PluginPlatform] }) platforms!: PluginPlatform[];
  @ApiProperty({ enum: ['zip', 'tar_gzip'] }) archive_format!: 'zip' | 'tar_gzip';
  @ApiProperty() sha256!: string;
  @ApiProperty({ type: Number }) size_bytes!: number;
  @ApiProperty({ enum: ['created', 'replaced', 'unchanged'] }) result!: 'created' | 'replaced' | 'unchanged';
}

export class PackageCurrent {
  @ApiProperty() name!: string;
  @ApiProperty() version!: string;
  @ApiProperty({ type: [PluginPlatform] }) platforms!: PluginPlatform[];
  @ApiProperty({ enum: ['zip', 'tar_gzip'] }) archive_format!: 'zip' | 'tar_gzip';
  @ApiProperty() sha256!: string;
  @ApiProperty({ type: Number }) size_bytes!: number;
}

export class PluginVersionSummary {
  @ApiProperty() version!: string;
  @ApiProperty({ type: [PluginPlatform] }) platforms!: PluginPlatform[];
  @ApiProperty() min_host_version!: string;
  @ApiProperty({ type: [String] }) skills!: string[];
  @ApiProperty({ type: [String] }) mcp!: string[];
  @ApiProperty({ enum: ['zip', 'tar_gzip'] }) archive_format!: 'zip' | 'tar_gzip';
  @ApiProperty() sha256!: string;
  @ApiProperty({ type: Number }) size_bytes!: number;
}

export class PluginConfigurationRequirement {
  @ApiProperty() name!: string;
  @ApiProperty() label!: string;
  @ApiProperty() required!: boolean;
  @ApiProperty() secret!: boolean;
}

export class PluginExternalSoftwareRequirement {
  @ApiProperty() name!: string;
  @ApiProperty() display_name!: string;
  @ApiProperty() instructions!: string;
  @ApiPropertyOptional() download_url?: string;
  @ApiPropertyOptional({ type: [PluginPlatform] }) platforms?: PluginPlatform[];
}

export class PluginVersionDetail extends PluginVersionSummary {
  @ApiProperty() updated_at!: string;
  @ApiProperty({ type: [PluginConfigurationRequirement] }) configuration!: PluginConfigurationRequirement[];
  @ApiProperty({ type: [PluginExternalSoftwareRequirement] }) external_software!: PluginExternalSoftwareRequirement[];
}

export class PluginResolvedPackage extends PluginVersionSummary {
  @ApiProperty() name!: string;
}

export class PluginRecord {
  @ApiProperty() name!: string;
  @ApiProperty() display_name!: string;
  @ApiProperty() description!: string;
  @ApiProperty({ enum: ['unpublished', 'listed', 'deprecated'] }) publication!:
    'unpublished' | 'listed' | 'deprecated';
  @ApiProperty({ type: [PluginVersionSummary] }) versions!: PluginVersionSummary[];
}

export class PluginDetail extends PluginRecord {
  @ApiProperty({ type: [PluginVersionDetail] }) declare versions: PluginVersionDetail[];
  @ApiProperty({ type: [String] }) dependent_agents!: string[];
}

export class PluginDescriptionInput {
  @ApiProperty() display_name!: string;
  @ApiProperty() description!: string;
}

export class PluginPublicationInput {
  @ApiProperty() listed!: boolean;
}

export class PluginResolveInput {
  @ApiProperty({ enum: ['macos', 'windows', 'linux'] }) os!: PluginPlatform['os'];
  @ApiProperty({ enum: ['x64', 'arm64'] }) arch!: PluginPlatform['arch'];
  @ApiProperty() host_version!: string;
  @ApiPropertyOptional() installed_version?: string;
  @ApiPropertyOptional({ type: [String], default: [] }) required_skills?: string[];
  @ApiPropertyOptional({ type: [String], default: [] }) required_mcp?: string[];
}

export class PluginResolution {
  @ApiProperty({ type: PluginResolvedPackage }) package!: PluginResolvedPackage;
  @ApiProperty() update_available!: boolean;
}
