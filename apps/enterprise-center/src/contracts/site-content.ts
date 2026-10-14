import { ApiProperty, ApiPropertyOptional } from '@nestjs/swagger';

export const siteNodeKinds = ['text', 'paragraph', 'heading', 'strong', 'em', 's', 'blockquote', 'bullet_list', 'ordered_list', 'list_item', 'code', 'code_inline', 'link', 'image', 'break', 'hr', 'table', 'thead', 'tbody', 'tr', 'th', 'td'] as const;
/** 白名单阅读节点，不携带 HTML、样式或组件代码；前端仅消费生成契约。 */
export class SiteNode {
  @ApiProperty({ enum: siteNodeKinds }) kind!: typeof siteNodeKinds[number];
  @ApiPropertyOptional({ type: String }) text?: string;
  @ApiPropertyOptional({ type: String }) href?: string;
  @ApiPropertyOptional({ type: String }) id?: string;
  @ApiPropertyOptional({ type: Number }) level?: number;
  @ApiPropertyOptional({ type: Number }) start?: number;
  @ApiPropertyOptional({ type: () => [SiteNode] }) children?: SiteNode[];
}
export class SiteArticle {
  @ApiProperty({ type: String }) id!: string;
  @ApiProperty({ type: String }) title!: string;
  @ApiProperty({ type: String }) date!: string;
  @ApiProperty({ type: String }) applies_to!: string;
  @ApiProperty({ type: [SiteNode] }) body!: SiteNode[];
}
export class SiteChapter {
  @ApiProperty({ type: String }) title!: string;
  @ApiProperty({ type: [SiteArticle] }) articles!: SiteArticle[];
}
export class SiteScenario {
  @ApiProperty({ type: String }) id!: string;
  @ApiProperty({ type: String }) label!: string;
  @ApiProperty({ type: String }) plugin!: string;
  @ApiProperty({ type: String }) prompt!: string;
  @ApiProperty({ type: String }) intro!: string;
  @ApiProperty({ type: String }) title!: string;
  @ApiProperty({ type: String }) body!: string;
  @ApiProperty({ type: String }) file!: string;
  @ApiProperty({ type: String }) reply!: string;
  @ApiProperty({ type: [String] }) steps!: string[];
  @ApiProperty({ enum: ['drawing', 'scene', 'quote', 'notes'] }) kind!: 'drawing' | 'scene' | 'quote' | 'notes';
}
export class SiteHomeSection {
  @ApiPropertyOptional({ type: [SiteScenario] }) scenarios?: SiteScenario[];
  @ApiProperty({ enum: ['hero', 'workspace', 'capabilities', 'modes', 'resources', 'closing'] }) kind!: 'hero' | 'workspace' | 'capabilities' | 'modes' | 'resources' | 'closing';
  @ApiProperty({ type: String }) title!: string;
  @ApiProperty({ type: String }) description!: string;
  @ApiProperty({ type: [SiteNode] }) body!: SiteNode[];
}
export class SiteAsset {
  @ApiProperty({ type: String }) id!: string;
  @ApiProperty({ enum: ['image/png', 'image/jpeg', 'image/webp'] }) type!: string;
  @ApiProperty({ type: Number }) size!: number;
  @ApiProperty({ type: String }) sha256!: string;
}
export class SiteContent {
  @ApiProperty({ enum: [1] }) format!: 1;
  @ApiProperty({ type: [SiteHomeSection] }) home!: SiteHomeSection[];
  @ApiProperty({ type: [SiteChapter] }) manual!: SiteChapter[];
  @ApiProperty({ type: [SiteAsset] }) assets!: SiteAsset[];
}
export class SiteChangelogEntry {
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) published_at!: string;
  @ApiProperty({ type: [SiteNode] }) body!: SiteNode[];
}
export class SiteNotesPreviewInput {
  @ApiProperty({ type: String, description: 'Markdown 正文，最多 16 KiB UTF-8' }) notes!: string;
}

export class SiteDownloadQuery {
  @ApiPropertyOptional({ type: String }) version?: string;
}
export class SiteDownload {
  @ApiProperty({ enum: ['darwin-aarch64', 'windows-x86_64', 'windows-aarch64'] }) target!: string;
  @ApiProperty({ type: String }) version!: string;
  @ApiProperty({ type: String }) published_at!: string;
  @ApiProperty({ type: String }) name!: string;
  @ApiProperty({ type: Number }) size!: number;
  @ApiProperty({ type: String }) sha256!: string;
  @ApiProperty({ type: String }) url!: string;
}
