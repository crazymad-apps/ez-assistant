import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { PluginPlatform } from './plugins.js';

export class AgentCapabilityReference {
  @ApiProperty() name!: string;
  @ApiPropertyOptional() plugin?: string;
}

export class AgentConfig {
  @ApiProperty() name!: string;
  @ApiProperty() description!: string;
  @ApiProperty() prompt!: string;
  @ApiProperty({ type: [AgentCapabilityReference] }) skills!: AgentCapabilityReference[];
  @ApiProperty({ type: [AgentCapabilityReference] }) mcp!: AgentCapabilityReference[];
}

export class AgentDependencyDiagnostic {
  @ApiProperty({ enum: ['skill', 'mcp'] }) kind!: 'skill' | 'mcp';
  @ApiProperty() name!: string;
  @ApiPropertyOptional() plugin?: string;
  @ApiProperty({ enum: ['available', 'host_check_required', 'plugin_missing',
    'plugin_unpublished', 'plugin_deprecated', 'capability_missing'] })
  status!: 'available' | 'host_check_required' | 'plugin_missing' |
    'plugin_unpublished' | 'plugin_deprecated' | 'capability_missing';
  @ApiProperty({ type: [PluginPlatform] }) platforms!: PluginPlatform[];
}

export class AgentDetail extends AgentConfig {
  @ApiProperty({ type: [AgentDependencyDiagnostic] }) diagnostics!: AgentDependencyDiagnostic[];
}

export class AgentSummary {
  @ApiProperty() name!: string;
  @ApiProperty() description!: string;
  @ApiProperty() skill_count!: number;
  @ApiProperty() mcp_count!: number;
  @ApiProperty({ type: [AgentDependencyDiagnostic] }) diagnostics!: AgentDependencyDiagnostic[];
}
