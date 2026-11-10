import { readFileSync } from 'node:fs';
import type { INestApplication } from '@nestjs/common';
import { DocumentBuilder, SwaggerModule } from '@nestjs/swagger';

/** 在线文档和离线导出共用 Controller/DTO 元数据，不维护第二份手写 schema。 */
export function createOpenApiDocument(app: INestApplication) {
  const { version } = JSON.parse(readFileSync(new URL('./package-info.json', import.meta.url), 'utf8')) as { version: string };
  const config = new DocumentBuilder().setTitle('Enterprise Center API')
    .setDescription('企业中心 HTTP 接口。F01 M0 仅开放身份、组织与系统管理入口，尚未组织化的业务暂不可用。')
    .setVersion(version)
    .addBearerAuth({ type: 'http', scheme: 'bearer', bearerFormat: 'JWT', description: '有效期 7 天；中心重启需重新登录。凭据仅从 Bearer 请求头读取。' }, 'tokenBearer')
    .addSecurityRequirements('tokenBearer')
    .build();
  return SwaggerModule.createDocument(app, config);
}
