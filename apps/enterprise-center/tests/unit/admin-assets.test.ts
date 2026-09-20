import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { checkAdminAssets } from '../../dist/admin-assets.js';
import { createHttpApp } from '../../dist/app.js';
import { startService } from '../../dist/service.js';
import { loadConfig } from '../../dist/config.js';

let directory: string;
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
function manifest(value: unknown) {
  writeFileSync(join(directory, '.vite/manifest.json'), JSON.stringify(value));
}
const entry = { file: 'assets/app.js', isEntry: true, css: ['assets/app.css'] };
beforeEach(() => {
  directory = mkdtempSync(join(tmpdir(), 'ez-center-static-test-'));
  mkdirSync(join(directory, 'assets'));
  mkdirSync(join(directory, '.vite'));
  writeFileSync(
    join(directory, 'index.html'),
    '<html><head><script type="module" src="/admin/assets/app.js"></script><link rel="stylesheet" href="/admin/assets/app.css"></head><body><div id="root"></div></body></html>',
  );
  writeFileSync(join(directory, 'assets/app.js'), 'document.title = "Enterprise Center";');
  writeFileSync(join(directory, 'assets/app.css'), 'body { margin: 0; }');
  manifest({ 'index.html': entry });
});
afterEach(async () => {
  await app?.close();
  app = undefined;
  // 只清理由本用例创建的临时资源，不触碰真实构建或用户目录。
  rmSync(directory, { recursive: true, force: true });
});

describe('正式后台资源准入', () => {
  it('认可 Vite 入口和全部资源，不改写文件', async () => {
    const before = readFileSync(join(directory, '.vite/manifest.json'), 'utf8');
    await expect(checkAdminAssets(directory)).resolves.toBeUndefined();
    expect(readFileSync(join(directory, '.vite/manifest.json'), 'utf8')).toBe(before);
  });
  it.each(['index.html', '.vite/manifest.json', 'assets/app.js', 'assets/app.css'])(
    '缺少 %s 明确失败',
    async (file) => {
      rmSync(join(directory, file));
      await expect(checkAdminAssets(directory)).rejects.toMatchObject({ code: 'ADMIN_ASSETS_INVALID' });
    },
  );
  it('缺少延迟加载资源同样拒绝，不只检查首页', async () => {
    manifest({ 'index.html': { ...entry, dynamicImports: ['lazy'] }, lazy: { file: 'assets/lazy.js' } });
    await expect(checkAdminAssets(directory)).rejects.toMatchObject({ code: 'ADMIN_ASSETS_INVALID' });
  });
  it('错误清单、越界路径、软链和空文件都拒绝且错误不带磁盘路径', async () => {
    manifest({ 'index.html': { ...entry, assets: ['assets/../outside.txt'] } });
    await expect(checkAdminAssets(directory)).rejects.not.toThrow(directory);
    manifest({ 'index.html': { ...entry, imports: ['missing-chunk'] } });
    await expect(checkAdminAssets(directory)).rejects.toMatchObject({ code: 'ADMIN_ASSETS_INVALID' });
    manifest({ 'index.html': entry });
    rmSync(join(directory, 'assets/app.js'));
    symlinkSync(join(directory, 'index.html'), join(directory, 'assets/app.js'));
    await expect(checkAdminAssets(directory)).rejects.toMatchObject({ code: 'ADMIN_ASSETS_INVALID' });
    rmSync(join(directory, 'assets/app.js'));
    writeFileSync(join(directory, 'assets/app.js'), '');
    await expect(checkAdminAssets(directory)).rejects.toMatchObject({ code: 'ADMIN_ASSETS_INVALID' });
  });
  it('资源失败优先于数据库连接/升级', async () => {
    const config = loadConfig({
      CENTER_DATABASE_URL: 'postgresql://unavailable:secret@127.0.0.1:1/not_connected',
      CENTER_PUBLIC_ORIGIN: 'http://127.0.0.1:7320',
      CENTER_ALLOW_HTTP_LOOPBACK: 'true',
    });
    await expect(startService(config, join(directory, 'missing'))).rejects.toMatchObject({
      code: 'ADMIN_ASSETS_INVALID',
    });
  });
  it('默认进程入口要求完整后台，不静默回退 API-only', () => {
    cpSync('dist', join(directory, 'dist'), { recursive: true });
    symlinkSync(join(process.cwd(), 'node_modules'), join(directory, 'node_modules'), 'dir');
    writeFileSync(join(directory, 'package.json'), '{"type":"module"}');
    const child = spawnSync(process.execPath, [join(directory, 'dist/main.js')], {
      timeout: 5000,
      env: {
        ...process.env,
        CENTER_DATABASE_URL: 'postgresql://unavailable:secret@127.0.0.1:1/not_connected',
        CENTER_PUBLIC_ORIGIN: 'http://127.0.0.1:7320',
        CENTER_ALLOW_HTTP_LOOPBACK: 'true',
      },
      encoding: 'utf8',
    });
    expect(child.status).toBe(1);
    expect(child.stdout).toBe('');
    expect(child.stderr).toContain('ADMIN_ASSETS_INVALID');
    expect(child.stderr).not.toContain('secret');
  });
});

describe('同源 HTTP 静态装配', () => {
  it('公开后台入口/资源，API 保留原契约，Swagger 可共存', async () => {
    app = await createHttpApp(undefined, directory);
    const page = await app.inject({ method: 'GET', url: '/admin/' });
    expect(page.statusCode).toBe(200);
    expect(page.headers['content-type']).toContain('text/html');
    expect(page.body).toContain('id="root"');
    expect(page.headers['cache-control']).toBe('no-store');
    expect(page.headers['content-security-policy']).toContain("script-src 'self'");
    expect(page.headers['content-security-policy']).toContain("frame-ancestors 'none'");
    expect(page.headers['x-content-type-options']).toBe('nosniff');
    const redirect = await app.inject({ method: 'GET', url: '/admin' });
    expect(redirect.statusCode).toBe(301);
    expect(redirect.headers.location).toBe('/admin/');
    for (const file of ['app.js', 'app.css'])
      expect((await app.inject({ method: 'GET', url: `/admin/assets/${file}` })).statusCode).toBe(200);
    expect((await app.inject({ method: 'GET', url: '/api/info' })).json().software_version).toBe('0.1.0');
    expect((await app.inject({ method: 'GET', url: '/api/docs/swagger-ui.css' })).statusCode).toBe(200);
    expect((await app.inject({ method: 'GET', url: '/api/users' })).statusCode).not.toBe(200);
  });
  it.each(['/admin/.vite/manifest.json', '/admin/.env', '/admin/unknown', '/admin/assets/missing.js', '/api/missing'])(
    '不向 %s 回落首页或暴露内部文件',
    async (url) => {
      app = await createHttpApp(undefined, directory);
      const result = await app.inject({ method: 'GET', url });
      expect(result.statusCode).toBe(404);
      expect(result.body).not.toContain('<html>');
    },
  );
  it('拒绝路径穿越与静态写请求', async () => {
    app = await createHttpApp(undefined, directory);
    for (const url of ['/admin/assets/%2e%2e/%2e%2e/.env', '/admin/assets/../.vite/manifest.json']) {
      expect((await app.inject({ method: 'GET', url })).statusCode).not.toBe(200);
    }
    expect((await app.inject({ method: 'POST', url: '/admin/' })).statusCode).not.toBe(200);
  });
});
