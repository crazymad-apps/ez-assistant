import { createClient } from '@hey-api/openapi-ts';
import { mkdtemp, readdir, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

// 沿用参考工程的生成器/axios 插件和环境变量；ESM 工程不使用 CommonJS require。
const input = process.env.OPENAPI_INPUT || '../enterprise-center/openapi.json';
const output = process.env.OPENAPI_OUTPUT || 'src/request/openapi';
const check = process.argv.includes('--check');
const temporary = check ? await mkdtemp(join(tmpdir(), 'ez-admin-openapi-')) : undefined;
async function files(directory, prefix = '') {
  const result = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.isDirectory()) result.push(...(await files(join(directory, entry.name), prefix + entry.name + '/')));
    else result.push(prefix + entry.name);
  }
  return result.sort();
}
try {
  await createClient({ input, output: temporary || output, plugins: ['@hey-api/client-axios'] });
  if (temporary) {
    const actual = await files(output),
      expected = await files(temporary);
    if (JSON.stringify(actual) !== JSON.stringify(expected))
      throw new Error('OpenAPI 生成文件集合已过期，请运行 npm run genapi');
    for (const file of expected) {
      if (!(await readFile(join(output, file))).equals(await readFile(join(temporary, file)))) {
        throw new Error('OpenAPI 生成内容已过期，请运行 npm run genapi');
      }
    }
    console.log('OpenAPI 生成内容一致');
  }
} finally {
  // 只删除本次 mkdtemp 创建的生成校验目录，不触碰用户配置的输出目录。
  if (temporary && resolve(temporary).startsWith(join(tmpdir(), 'ez-admin-openapi-')))
    await rm(temporary, { recursive: true });
}
