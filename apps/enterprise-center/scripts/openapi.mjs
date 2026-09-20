import { readFileSync, writeFileSync } from 'node:fs';
import { createHttpApp } from '../dist/app.js';
import { createOpenApiDocument } from '../dist/openapi.js';

const mode = process.argv[2];
if (!['export', 'check'].includes(mode) || process.argv.length !== 3) throw new Error('用法：openapi.mjs export|check');
// 只装配 HTTP 元数据，不调用持有数据库的 startService，也不监听端口。
const app = await createHttpApp();
try {
  const output = JSON.stringify(createOpenApiDocument(app), null, 2) + '\n';
  const target = new URL('../openapi.json', import.meta.url);
  if (mode === 'export') writeFileSync(target, output);
  else if (readFileSync(target, 'utf8') !== output) throw new Error('OpenAPI 快照已过期，请执行 npm run openapi:export');
  console.log(mode === 'export' ? '已生成 openapi.json' : 'OpenAPI 快照一致');
} finally { await app.close(); }
