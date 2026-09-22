import { ApiProperty } from '@nestjs/swagger';

// class 为 Swagger 提供运行时元数据；Controller 返回值与文档使用同一 DTO。
export class CenterInfo {
  @ApiProperty({ type: [String], description: '已实现能力；代理就绪后才声明 llm_proxy' })
  readonly capabilities!: string[];

  @ApiProperty({ type: String, description: 'Center 软件版本，与 Runtime 独立发布', example: '0.1.0' })
  readonly software_version!: string;

  @ApiProperty({ type: 'integer', minimum: 1, description: '当前企业协议版本', example: 1 })
  readonly protocol_version!: number;

  @ApiProperty({ type: 'integer', minimum: 1, description: '最低兼容企业协议版本', example: 1 })
  readonly min_protocol_version!: number;
}
