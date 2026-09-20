import { readFileSync } from 'node:fs';
import type { INestApplication } from '@nestjs/common';
import { DocumentBuilder, SwaggerModule } from '@nestjs/swagger';

/** 在线文档和离线导出共用 Controller/DTO 元数据，不维护第二份手写 schema。 */
export function createOpenApiDocument(app: INestApplication) {
  const { version } = JSON.parse(readFileSync(new URL('./package-info.json', import.meta.url), 'utf8')) as { version: string };
  const config = new DocumentBuilder().setTitle('Enterprise Center API')
    .setDescription('企业中心已实现的 HTTP 接口；软件版本不代表 Runtime 版本。')
    .setVersion(version)
    .addBearerAuth({ type: 'http', scheme: 'bearer', description: 'ct_ 内存 Token；不接受 cl_ LLM key 或 Cookie；中心重启需重新登录' }, 'tokenBearer')
    .addSecurityRequirements('tokenBearer')
    .build();
  return SwaggerModule.createDocument(app, config);
}
