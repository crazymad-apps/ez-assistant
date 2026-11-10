import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';
import { PageQuery } from './users.js';

export class Organization {
  @ApiProperty({ type: String, format: 'uuid' }) readonly id!: string;
  @ApiProperty({ type: String }) readonly code!: string;
  @ApiProperty({ type: String }) readonly name!: string;
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}

export class CreateOrganization {
  @ApiProperty({ type: String, pattern: '^[a-z0-9][a-z0-9_-]{0,63}$' }) readonly code!: string;
  @ApiProperty({ type: String, minLength: 1, maxLength: 64 }) readonly name!: string;
  @ApiProperty({ type: Number }) readonly administrator_user_id!: number;
}

export class UpdateOrganization {
  @ApiProperty({ type: String, pattern: '^[a-z0-9][a-z0-9_-]{0,63}$' }) readonly code!: string;
  @ApiProperty({ type: String, minLength: 1, maxLength: 64 }) readonly name!: string;
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}

export class SaveMember {
  @ApiProperty({ enum: ['member', 'admin'] }) readonly role!: 'member' | 'admin';
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}

export class UpdateMember {
  @ApiPropertyOptional({ enum: ['member', 'admin'] }) readonly role?: SaveMember['role'];
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
}

export class OrganizationMember extends SaveMember {
  @ApiProperty({ type: Number }) readonly user_id!: number;
  @ApiProperty({ type: String }) readonly username!: string;
  @ApiProperty({ type: String }) readonly display_name!: string;
  @ApiProperty({ type: Boolean }) readonly account_enabled!: boolean;
}

export class MemberQuery extends PageQuery {
  @ApiPropertyOptional({ type: Boolean }) readonly active_only?: boolean;
  @ApiPropertyOptional({ type: String, maxLength: 64 }) readonly search?: string;
  @ApiPropertyOptional({ enum: ['member', 'admin'] }) readonly role?: SaveMember['role'];
  @ApiPropertyOptional({ type: Boolean }) readonly enabled?: boolean;
}

export class MemberPage {
  @ApiProperty({ type: [OrganizationMember] }) readonly items!: OrganizationMember[];
  @ApiProperty({ type: Number }) readonly total!: number;
  @ApiProperty({ type: Number }) readonly limit!: number;
  @ApiProperty({ type: Number }) readonly offset!: number;
}

export class MemberCandidateQuery {
  @ApiProperty({ type: String, minLength: 3, maxLength: 64 }) readonly username!: string;
}

export class MemberCandidate {
  @ApiProperty({ type: Number }) readonly id!: number;
  @ApiProperty({ type: String }) readonly username!: string;
  @ApiProperty({ type: String }) readonly display_name!: string;
  @ApiProperty({ type: Boolean }) readonly enabled!: boolean;
}
