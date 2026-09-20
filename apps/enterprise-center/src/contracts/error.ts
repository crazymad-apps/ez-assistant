import { ApiProperty } from '@nestjs/swagger';

export class CenterErrorDetail {
  @ApiProperty({ type: String, description: '稳定原因码', example: 'SERVICE_UNAVAILABLE' })
  readonly code!: string;

  @ApiProperty({ type: String, description: '脱敏错误说明' })
  readonly message!: string;

  @ApiProperty({ type: String, format: 'uuid', description: '服务端生成的请求关联标识' })
  readonly request_id!: string;
}

export class CenterErrorResponse {
  @ApiProperty({ type: CenterErrorDetail })
  readonly error!: CenterErrorDetail;
}
