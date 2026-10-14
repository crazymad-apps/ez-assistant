import { expect, it } from 'vitest';
import { resolve } from 'node:path';
import { readFile } from 'node:fs/promises';
import { createHttpApp } from '../../dist/app.js';
import { checkSiteAssets } from '../../dist/admin-assets.js';
import { startService } from '../../dist/service.js';
import { loadConfig } from '../../dist/config.js';

it('正式页面独立匹配，缺资源和未知 API 保留错误，后台共存', async () => {
  const root = resolve('../enterprise-admin/dist/site');
  await checkSiteAssets(root);
  const app = await createHttpApp(undefined, resolve('../enterprise-admin/dist/admin'), root);
  try {
    for (const url of ['/', '/download?version=0.31.0', '/changelog', '/manual', '/manual/getting-started', '/admin/']) {
      const reply = await app.inject({ url });
      expect(reply.statusCode, url).toBe(200);
      expect(reply.headers['content-type']).toContain('text/html');
    }
    for (const url of ['/api', '/api/does-not-exist', '/admin/missing.js', '/unknown', '/manual/nested/path', '/manual/Bad_Name', '/site-assets/missing.js', '/site-assets/.vite/manifest.json', '/site-content/missing', '/.vite/manifest.json']) {
      const reply = await app.inject({ url });
      expect(reply.statusCode, url).toBe(404);
      expect(reply.headers['content-type'] ?? '', url).not.toContain('text/html');
    }
    const manifest = JSON.parse(await readFile(`${root}/.vite/manifest.json`, 'utf8'));
    const reply = await app.inject({ url: `/${manifest['index.html'].file}` });
    expect(reply.statusCode).toBe(200);
    expect(reply.headers['content-type']).toContain('javascript');
    expect((await app.inject({ url: '/api/site/downloads' })).statusCode).toBe(503);
  } finally { await app.close(); }
});

it('完整模式在连接数据库前拒绝缺少官网产物', async () => {
  const config = loadConfig({ CENTER_DATABASE_URL: 'postgresql://unused@127.0.0.1:1/unreachable', CENTER_PUBLIC_ORIGIN: 'http://127.0.0.1:7343' });
  // 已有后台夹具测试覆盖缺后台；这里复用真实后台校验，刻意缺官网。
  const { mkdtemp, cp, rm } = await import('node:fs/promises');
  const root = await mkdtemp(resolve('../../.runtime-test/f01-assets-'));
  try {
    await cp(resolve('../enterprise-admin/dist/admin'), `${root}/admin`, { recursive: true });
    await expect(startService(config, `${root}/admin`)).rejects.toMatchObject({ code: 'SITE_ASSETS_INVALID' });
  } finally { await rm(root, { recursive: true, force: true }); }
});
