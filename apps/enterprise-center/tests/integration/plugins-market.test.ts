import { createHash, randomBytes } from 'node:crypto';
import { createWriteStream } from 'node:fs';
import { copyFile, mkdir, readFile, rename, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { beforeAll, afterAll, expect, it } from 'vitest';
import { ZipFile } from 'yazl';
import { createHttpApp } from '../../dist/app.js';
import { createDataSource } from '../../dist/database/source.js';
import { createPool } from '../../dist/database/pool.js';
import { checkDatabase } from '../../dist/database/check.js';

const url = process.env.CENTER_TEST_DATABASE_URL;
const adminUrl = process.env.CENTER_TEST_ADMIN_URL;
const pluginRoot = process.env.CENTER_TEST_PLUGIN_ROOT;
const backupRoot = process.env.CENTER_TEST_BACKUP_ROOT;
const pgBin = process.env.CENTER_TEST_PG_BIN;
if (!url || !adminUrl || !pluginRoot || !backupRoot || !pgBin || process.env.CENTER_TEST_ALLOW_WRITE !== 'true')
  throw new Error('须显式指定已确认的 C01 M1 新隔离库、包根、备份根及写入授权');
const target = new URL(url), adminTarget = new URL(adminUrl);
const scenario = resolve('../..', '.runtime-test');
if (target.hostname !== '127.0.0.1' || target.username !== 'center_app' ||
    !/^\/ez_center_c01_m1_[a-z0-9]+$/.test(target.pathname) ||
    adminTarget.hostname !== target.hostname || adminTarget.port !== target.port ||
    adminTarget.pathname !== '/postgres' || adminTarget.username !== 'postgres' ||
    !resolve(pluginRoot).startsWith(`${scenario}/c01-m1-`) ||
    !resolve(backupRoot).startsWith(`${scenario}/c01-m1-`))
  throw new Error('目标不是独立 C01 M1 场景');

const run = promisify(execFile);
const pool = createPool({ url });
const admin = createPool({ url: adminUrl });
const source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>>;
let origin: string;

async function archive(path: string, version: string, content: string) {
  const zip = new ZipFile();
  zip.addBuffer(Buffer.from(JSON.stringify({
    format_version: 1, name: 'document-tools', display_name: '文档工具', description: '读取文档',
    version, platforms: [{ os: 'macos', arch: 'arm64' }], min_host_version: '0.28.0',
    skills: ['document-review'], mcp: [], configuration: [], external_software: [],
  })), 'plugin.json');
  zip.addBuffer(Buffer.from('---\nname: document-review\ndescription: Review documents\n---\n' + content),
    'skills/document-review/SKILL.md');
  // 大于本机 socket/Fetch 缓冲的不可压缩资源，使下载保持在途以验证替换与下架竞态。
  zip.addBuffer(randomBytes(16 * 1024 * 1024), 'resources/payload.bin', { compress: false });
  zip.end();
  await pipeline(zip.outputStream, createWriteStream(path));
}

async function count(table: string): Promise<string> {
  return (await pool.query<{ count: string }>(`SELECT count(*) FROM public.${table}`)).rows[0]!.count;
}

beforeAll(async () => {
  const targetRow = await pool.query(`SELECT current_user AS role,current_database() AS database,
    host(inet_server_addr()) AS host,inet_server_port() AS port`);
  expect(targetRow.rows[0]).toEqual({ role: 'center_app', database: target.pathname.slice(1),
    host: target.hostname, port: Number(target.port) });
  expect((await checkDatabase(pool)).schema_version).toBe(3);
  expect(await count('plugins')).toBe('0');
  expect(await count('plugin_packages')).toBe('0');
  expect(await count('management_audit')).toBe('1');
  await source.initialize();
  app = await createHttpApp({ source, origin: `http://127.0.0.1:${target.port}`, pluginRoot });
  await app.listen(0, '127.0.0.1');
  origin = await app.getUrl();
  await mkdir(backupRoot, { recursive: true, mode: 0o700 });
});

afterAll(async () => {
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
  await admin.end();
});

it('发布、条件写、选包、SHA 条件下载、替换与废弃形成真实 HTTP 闭环', async () => {
  const request = (path: string, token: string, options: RequestInit = {}) =>
    fetch(`${origin}/api${path}`, { ...options, headers: { authorization: `Bearer ${token}`,
      ...options.headers } });
  const adminLogin = await fetch(`${origin}/api/auth/login`, { method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: '123456' }) });
  expect(adminLogin.status).toBe(200);
  const adminToken = (await adminLogin.json() as { token: string }).token;
  const createdUser = await request('/users', adminToken, { method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'reader', display_name: '读取者', role: 'user',
      enabled: true, password: 'reader-password-123' }) });
  expect(createdUser.status).toBe(201);
  const userLogin = await fetch(`${origin}/api/auth/login`, { method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'reader', password: 'reader-password-123' }) });
  expect(userLogin.status).toBe(200);
  const userToken = (await userLogin.json() as { token: string }).token;

  const first = join(backupRoot, 'market-first.zip');
  const second = join(backupRoot, 'market-second.zip');
  await archive(first, '1.0.0', 'original package');
  await archive(second, '1.0.0', 'replacement package');
  const upload = (bytes: Buffer) => request('/plugin-packages', adminToken, { method: 'POST',
    headers: { 'content-type': 'application/zip' }, body: new Uint8Array(bytes) });
  const initialBytes = await readFile(first);
  const created = await upload(initialBytes);
  expect(created.status).toBe(201);
  const initial = await created.json() as { sha256: string };
  expect(createHash('sha256').update(initialBytes).digest('hex')).toBe(initial.sha256);
  await expect((await request('/runtime/plugins', userToken)).json()).resolves.toEqual([]);
  expect((await request('/plugins', userToken)).status).toBe(403);

  const detailUrl = '/plugins/document-tools';
  const detail = await request(detailUrl, adminToken);
  expect(detail.status).toBe(200);
  const firstEtag = detail.headers.get('etag')!;
  expect((await detail.json() as { publication: string }).publication).toBe('unpublished');
  const withoutCondition = await request(detailUrl, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ display_name: '新名称', description: '新说明' }) });
  expect(withoutCondition.status).toBe(428);
  const edited = await request(detailUrl, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json', 'if-match': firstEtag },
    body: JSON.stringify({ display_name: '新名称', description: '新说明' }) });
  expect(edited.status).toBe(200);
  const editedEtag = edited.headers.get('etag')!;
  expect(editedEtag).not.toBe(firstEtag);
  const stale = await request(detailUrl, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json', 'if-match': firstEtag },
    body: JSON.stringify({ display_name: '旧表单', description: '旧值' }) });
  expect(stale.status).toBe(412);

  const listed = await request(`${detailUrl}/publication`, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json', 'if-match': editedEtag },
    body: JSON.stringify({ listed: true }) });
  expect(listed.status).toBe(200);
  expect((await listed.json() as { publication: string }).publication).toBe('listed');
  const listedEtag = listed.headers.get('etag')!;
  expect((await request('/runtime/plugins', userToken)).status).toBe(200);
  const resolve = (body: object) => request('/runtime/plugins/document-tools/resolve', userToken,
    { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
  const selector = { os: 'macos', arch: 'arm64', host_version: '0.28.0',
    required_skills: ['document-review'] };
  expect((await resolve({ ...selector, os: 'windows' })).status).toBe(409);
  expect((await resolve({ ...selector, host_version: '0.27.0' })).status).toBe(409);
  expect((await resolve({ ...selector, required_skills: ['missing'] })).status).toBe(409);
  const resolved = await resolve(selector);
  expect(resolved.status).toBe(200);
  expect((await resolved.json() as { package: { sha256: string }; update_available: boolean }))
    .toMatchObject({ package: { sha256: initial.sha256 }, update_available: false });
  const packageUrl = '/runtime/plugins/document-tools/versions/1.0.0/package';
  expect((await request(packageUrl, userToken)).status).toBe(428);
  const downloaded = await request(packageUrl, userToken, { headers: { 'if-match': `"${initial.sha256}"` } });
  expect(downloaded.status).toBe(200);
  expect(downloaded.headers.get('x-plugin-sha256')).toBe(initial.sha256);
  const downloadedBytes = Buffer.from(await downloaded.arrayBuffer());
  expect(downloadedBytes.length).toBe(initialBytes.length);
  expect(createHash('sha256').update(downloadedBytes).digest('hex')).toBe(initial.sha256);

  // 覆盖当前引用前独立备份数据库与原包，并恢复到另一新库核对关键表。
  const dump = join(backupRoot, 'before-market-replacement.dump');
  await run(join(pgBin, 'pg_dump'), ['-Fc', '-f', dump, url]);
  expect((await stat(dump)).size).toBeGreaterThan(0);
  const oldPackageBackup = join(backupRoot, `${initial.sha256}.zip`);
  await copyFile(join(pluginRoot, 'packages', `${initial.sha256}.zip`), oldPackageBackup);
  expect(createHash('sha256').update(await readFile(oldPackageBackup)).digest('hex')).toBe(initial.sha256);
  const restoreName = `ez_center_c01_m1_restore_${Date.now()}`;
  expect((await admin.query('SELECT 1 FROM pg_database WHERE datname=$1', [restoreName])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${restoreName}" OWNER center_app`);
  const restoreUrl = new URL(url); restoreUrl.pathname = `/${restoreName}`;
  await run(join(pgBin, 'pg_restore'), ['--no-owner', '--no-privileges', '-d', restoreUrl.href, dump]);
  const restored = createPool({ url: restoreUrl.href });
  try {
    for (const table of ['pgmigrations', 'center_identity', 'users', 'management_audit',
      'plugins', 'plugin_packages']) {
      const row = await restored.query<{ count: string }>(`SELECT count(*) FROM public.${table}`);
      expect(row.rows[0]!.count).toBe(await count(table));
    }
    const sha = await restored.query<{ sha256: string }>(
      "SELECT sha256 FROM public.plugin_packages WHERE plugin_name='document-tools' AND version='1.0.0'");
    expect(sha.rows[0]!.sha256).toBe(initial.sha256);
  } finally { await restored.end(); }

  const admittedOld = await request(packageUrl, userToken,
    { headers: { 'if-match': `"${initial.sha256}"` } });
  expect(admittedOld.status).toBe(200);
  const replaced = await upload(await readFile(second));
  expect(replaced.status).toBe(200);
  const newSha = (await replaced.json() as { sha256: string }).sha256;
  expect(newSha).not.toBe(initial.sha256);
  const admittedBytes = Buffer.from(await admittedOld.arrayBuffer());
  expect(admittedBytes.length).toBe(initialBytes.length);
  expect(createHash('sha256').update(admittedBytes).digest('hex')).toBe(initial.sha256);
  expect((await request(packageUrl, userToken, { headers: { 'if-match': `"${initial.sha256}"` } })).status)
    .toBe(412);
  const latest = await request(packageUrl, userToken, { headers: { 'if-match': `"${newSha}"` } });
  expect(latest.status).toBe(200);
  expect(createHash('sha256').update(Buffer.from(await latest.arrayBuffer())).digest('hex')).toBe(newSha);

  // 暂移当前文件仅验证读取失败语义，独立副本已在 backupRoot，随后无条件还原。
  const currentPath = join(pluginRoot, 'packages', `${newSha}.zip`);
  const heldPath = join(backupRoot, `${newSha}.zip`);
  await copyFile(currentPath, heldPath);
  expect(createHash('sha256').update(await readFile(heldPath)).digest('hex')).toBe(newSha);
  await rename(currentPath, join(backupRoot, 'temporarily-held.zip'));
  try {
    expect((await resolve(selector)).status).toBe(503);
    expect((await request(packageUrl, userToken, { headers: { 'if-match': `"${newSha}"` } })).status)
      .toBe(503);
  } finally { await rename(join(backupRoot, 'temporarily-held.zip'), currentPath); }

  const pendingDownload = await request(packageUrl, userToken,
    { headers: { 'if-match': `"${newSha}"` } });
  expect(pendingDownload.status).toBe(200);

  const deprecated = await request(`${detailUrl}/publication`, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json', 'if-match': listedEtag },
    body: JSON.stringify({ listed: false }) });
  expect(deprecated.status).toBe(200);
  let delivered = 0;
  try {
    for await (const chunk of pendingDownload.body!) delivered += chunk.length;
  } catch { /* 下架会中断已经准入但未完成的 HTTP 传输。 */ }
  expect(delivered).toBeLessThan(Number(pendingDownload.headers.get('content-length')));
  expect((await request(packageUrl, userToken, { headers: { 'if-match': `"${newSha}"` } })).status)
    .toBe(410);
  const status = await request('/runtime/plugins/document-tools', userToken);
  expect((await status.json() as { publication: string; versions: unknown[] }))
    .toMatchObject({ publication: 'deprecated', versions: [] });
  const relisted = await request(`${detailUrl}/publication`, adminToken, { method: 'PUT',
    headers: { 'content-type': 'application/json', 'if-match': deprecated.headers.get('etag')! },
    body: JSON.stringify({ listed: true }) });
  expect(relisted.status).toBe(200);
  expect(await count('plugins')).toBe('1');
  expect(await count('plugin_packages')).toBe('1');
});
