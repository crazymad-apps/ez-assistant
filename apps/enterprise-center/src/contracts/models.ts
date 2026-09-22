import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';

export const providerTypes = [
  'openai',
  'deepseek',
  'dashscope_api',
  'dashscope_plan',
  'moonshot',
  'zhipu',
  'vllm',
  'local',
] as const;
export const preferences = ['auto', 'responses', 'chat_completions'] as const;
export const protocols = ['open_ai_responses', 'open_ai_chat_completions'] as const;
export const supports = ['unknown', 'supported', 'unsupported'] as const;
export const efforts = ['low', 'medium', 'high', 'xhigh', 'max'] as const;
export type ProviderType = (typeof providerTypes)[number];
export type Protocol = (typeof protocols)[number];
export type Support = (typeof supports)[number];
export type Effort = (typeof efforts)[number];

export class TokenLimit {
  @ApiProperty({ enum: ['unknown', 'known', 'invalid'] }) state!: 'unknown' | 'known' | 'invalid';
  @ApiPropertyOptional({ type: Number, minimum: 1, maximum: Number.MAX_SAFE_INTEGER }) value?: number;
}
export class ToolChoice {
  @ApiProperty({ enum: supports }) auto!: Support;
  @ApiProperty({ enum: supports }) none!: Support;
  @ApiProperty({ enum: supports }) required!: Support;
  @ApiProperty({ enum: supports }) named!: Support;
}
export class ModelParameters {
  @ApiProperty({ type: TokenLimit }) context_window_tokens!: TokenLimit;
  @ApiProperty({ type: TokenLimit }) max_input_tokens!: TokenLimit;
  @ApiProperty({ type: TokenLimit }) max_output_tokens!: TokenLimit;
  @ApiProperty({ type: TokenLimit }) reasoning_max_input_tokens!: TokenLimit;
  @ApiProperty({ type: TokenLimit }) reasoning_max_output_tokens!: TokenLimit;
  @ApiProperty({ enum: supports }) streaming!: Support;
  @ApiProperty({ type: ToolChoice }) tool_choice!: ToolChoice;
  @ApiProperty({ enum: ['unknown', 'unsupported', 'native_tool_result', 'follow_up_user_message'] })
  tool_image_projection!: 'unknown' | 'unsupported' | 'native_tool_result' | 'follow_up_user_message';
  @ApiProperty({ enum: supports }) image_input!: Support;
  @ApiProperty({ enum: supports }) tool_calls!: Support;
  @ApiProperty({ enum: supports }) reasoning!: Support;
  @ApiProperty({ enum: ['unknown', 'unsupported', 'optional', 'always'] }) reasoning_mode!:
    'unknown' | 'unsupported' | 'optional' | 'always';
  @ApiProperty({ type: 'object', additionalProperties: { type: 'string' }, nullable: true })
  reasoning_efforts!: Partial<Record<Effort, string>> | null;
  @ApiProperty({ type: String, enum: [...efforts, null], nullable: true }) default_reasoning_effort!: Effort | null;
}
export class ProviderConnection {
  @ApiProperty() display_name!: string;
  @ApiProperty({ enum: providerTypes }) provider_type!: ProviderType;
  @ApiProperty() endpoint!: string;
  @ApiProperty({ enum: preferences }) protocol_preference!: (typeof preferences)[number];
  @ApiProperty() models_path!: string;
  @ApiProperty({ enum: ['openai', 'vllm', 'moonshot', 'dashscope_native'] }) discovery_format!:
    'openai' | 'vllm' | 'moonshot' | 'dashscope_native';
}
export class CredentialChange {
  @ApiProperty({ enum: ['unchanged', 'replace', 'clear'] }) mode!: 'unchanged' | 'replace' | 'clear';
  @ApiPropertyOptional({ type: String, writeOnly: true }) value?: string;
}
export class SaveProvider {
  @ApiProperty({ type: ProviderConnection }) connection!: ProviderConnection;
  @ApiProperty({ type: CredentialChange }) credential!: CredentialChange;
}
export class ProviderSummary {
  @ApiProperty({ format: 'uuid' }) provider_instance_id!: string;
  @ApiProperty({ type: ProviderConnection }) connection!: ProviderConnection;
  @ApiProperty() has_api_key!: boolean;
}
export class ModelSelection {
  @ApiProperty({ format: 'uuid' }) provider_instance_id!: string;
  @ApiProperty() model_id!: string;
}
export class ModelConfigurationQuery extends ModelSelection {
  @ApiProperty({ enum: ['online', 'manual'], default: 'online' }) origin!: 'online' | 'manual';
}
export class SaveModelConfiguration extends ModelConfigurationQuery {
  @ApiProperty({ type: ModelParameters }) parameters!: ModelParameters;
}
export class ModelConfiguration extends SaveModelConfiguration {
  @ApiProperty({ enum: ['fixed', 'online', 'template', 'unconfigured'] }) source!: string;
  @ApiProperty({ type: 'object', additionalProperties: { type: 'string' } }) field_sources!: Record<string, string>;
  @ApiProperty({ type: Number, nullable: true }) updated_at_ms!: number | null;
  @ApiProperty({ type: String, nullable: true }) template_document!: string | null;
  @ApiProperty({ type: String, nullable: true }) template_checked_on!: string | null;
  @ApiProperty() requires_configuration!: boolean;
}
export class ModelConfigurationSummary {
  @ApiProperty() uses_template!: boolean;
  @ApiProperty() requires_configuration!: boolean;
}
export class CatalogModel {
  @ApiPropertyOptional({ type: ModelConfigurationSummary }) configuration?: ModelConfigurationSummary;
  @ApiProperty() model_id!: string;
  @ApiProperty({ type: String, nullable: true }) display_name!: string | null;
  @ApiProperty({ type: ModelParameters }) metadata!: ModelParameters;
}
export class CatalogSnapshot {
  @ApiProperty({ format: 'uuid' }) provider_instance_id!: string;
  @ApiProperty({ type: [CatalogModel] }) models!: CatalogModel[];
  @ApiProperty({ type: Number, nullable: true }) refreshed_at_ms!: number | null;
  @ApiProperty() connection_changed!: boolean;
}
export class FixedConfigurationPage {
  @ApiProperty({ type: [ModelConfiguration] }) items!: ModelConfiguration[];
  @ApiProperty() total!: number;
  @ApiProperty() offset!: number;
  @ApiProperty() limit!: number;
}
export class ProviderUsage {
  @ApiProperty() fixed_config_count!: number;
  @ApiProperty() default_model!: boolean;
}
export class SaveModelSettings {
  @ApiProperty({ type: ModelSelection, nullable: true }) default_model!: ModelSelection | null;
}
export class ModelSettings extends SaveModelSettings {
  @ApiProperty({ enum: ['ready', 'unavailable'] }) state!: 'ready' | 'unavailable';
  @ApiProperty({ type: String, nullable: true }) reason!: string | null;
  @ApiProperty({ type: ModelConfiguration, nullable: true }) configuration!: ModelConfiguration | null;
}
export class TemplateStatus {
  @ApiProperty() available!: boolean;
  @ApiProperty() count!: number;
  @ApiProperty({ type: Number, nullable: true }) loaded_at_ms!: number | null;
  @ApiProperty({ type: String, nullable: true }) error!: string | null;
}
export class RuntimeModelConfiguration {
  @ApiProperty({ enum: ['ready', 'unavailable'] }) state!: 'ready' | 'unavailable';
  @ApiProperty({ format: 'uuid' }) center_id!: string;
  @ApiPropertyOptional() reason?: string;
  @ApiPropertyOptional() endpoint?: string;
  @ApiPropertyOptional({ enum: providerTypes }) provider_type?: ProviderType;
  @ApiPropertyOptional() provider_display_name?: string;
  @ApiPropertyOptional({ enum: protocols }) protocol?: Protocol;
  @ApiPropertyOptional() model_id?: string;
  @ApiPropertyOptional({ type: ModelParameters }) parameters?: ModelParameters;
}
