import { createWriteStream } from 'node:fs';
import { copyFile, mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { join, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { afterAll, beforeAll, expect, it } from 'vitest';
import { ZipFile } from 'yazl';
import { create as createTar } from 'tar';
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
  throw new Error('须显式提供新建 C01 M0 隔离库、管理员连接、包根、备份根及写入授权');
const target = new URL(url), adminTarget = new URL(adminUrl);
const scenario = resolve('../..', '.runtime-test');
if (target.hostname !== '127.0.0.1' || target.username !== 'center_app' ||
    !/^\/ez_center_c01_m0_[a-z0-9]+$/.test(target.pathname) ||
    !Number.isInteger(Number(target.port)) || Number(target.port) < 1024 ||
    adminTarget.hostname !== target.hostname || adminTarget.port !== target.port ||
    adminTarget.username !== 'postgres' || adminTarget.pathname !== '/postgres' ||
    !resolve(pluginRoot).startsWith(`${scenario}/c01-m0-`) ||
    !resolve(backupRoot).startsWith(`${scenario}/c01-m0-`))
  throw new Error('目标不属于新建的 C01 M0 隔离场景');

const run = promisify(execFile);
const pool = createPool({ url });
const admin = createPool({ url: adminUrl });
const source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>>;
let origin: string;

async function counts() {
  const result: Record<string, string> = {};
  for (const table of ['pgmigrations', 'center_identity', 'users', 'management_audit',
    'custom_agents', 'plugins', 'plugin_packages']) {
    result[table] = (await pool.query<{ count: string }>(`SELECT count(*) FROM public.${table}`)).rows[0]!.count;
  }
  return result;
}

async function archive(path: string, content: string) {
  const zip = new ZipFile();
  const manifest = { format_version: 1, name: 'document-tools', display_name: '文档工具',
    description: '读取文档', version: '1.0.0', platforms: [{ os: 'macos', arch: 'arm64' }],
    min_host_version: '0.28.0', skills: ['document-review'], mcp: [], configuration: [], external_software: [] };
  zip.addBuffer(Buffer.from(JSON.stringify(manifest)), 'plugin.json');
  zip.addBuffer(Buffer.from('---\nname: document-review\ndescription: Review documents\n---\n' + content),
    'skills/document-review/SKILL.md');
  zip.end();
  await pipeline(zip.outputStream, createWriteStream(path));
}

async function tarArchive(path: string) {
  const sourceRoot = join(backupRoot!, 'tar-source');
  await mkdir(join(sourceRoot, 'skills', 'document-review'), { recursive: true });
  await writeFile(join(sourceRoot, 'plugin.json'), JSON.stringify({
    format_version: 1, name: 'document-tools', display_name: '文档工具', description: '读取文档',
    version: '1.1.0', platforms: [{ os: 'macos', arch: 'arm64' }], min_host_version: '0.28.0',
    skills: ['document-review'], mcp: [], configuration: [], external_software: [],
  }));
  await writeFile(join(sourceRoot, 'skills', 'document-review', 'SKILL.md'),
    '---\nname: document-review\ndescription: Review documents\n---\nTAR.GZ package');
  await createTar({ file: path, cwd: sourceRoot, gzip: true, portable: true }, ['plugin.json', 'skills']);
}

beforeAll(async () => {
  const actual = await pool.query("SELECT host(inet_server_addr()) AS host, inet_server_port() AS port, current_database() AS database, current_user AS role");
  expect(actual.rows[0]).toEqual({ host: target.hostname, port: Number(target.port),
    database: target.pathname.slice(1), role: 'center_app' });
  expect((await checkDatabase(pool)).schema_version).toBe(3);
  expect(await counts()).toEqual({ pgmigrations: '3', center_identity: '1', users: '1',
    management_audit: '1', custom_agents: '0', plugins: '0', plugin_packages: '0' });
  await source.initialize();
  app = await createHttpApp({ source, origin: `http://127.0.0.1:${target.port}`,
    pluginRoot });
  await app.listen(0, '127.0.0.1');
  origin = await app.getUrl();
  await mkdir(backupRoot, { recursive: true, mode: 0o700 });
});

afterAll(async () => {
  await app?.close();
  if (source.isInitialized) await source.destroy();
  if (pool) await pool.end();
  if (admin) await admin.end();
});

it('真实 HTTP 上传、同 SHA 不变、备份恢复后异 SHA 替换及失败保留', async () => {
  const first = join(backupRoot, 'first.zip'), second = join(backupRoot, 'second.zip');
  await archive(first, 'Version one');
  await archive(second, 'Version two');
  const login = await fetch(`${origin}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: '123456' }) });
  expect(login.status).toBe(200);
  const token = (await login.json() as { token: string }).token;
  const send = (bytes: Buffer, bearer = token, contentType = 'application/zip') => fetch(`${origin}/api/plugin-packages`, {
    method: 'POST', headers: { authorization: `Bearer ${bearer}`, 'content-type': contentType },
    body: new Uint8Array(bytes),
  });
  const firstBytes = await readFile(first);
  const denied = await send(firstBytes, 'invalid');
  expect(denied.status).toBe(401);
  expect(await counts()).toMatchObject({ plugins: '0', plugin_packages: '0' });

  const created = await send(firstBytes);
  expect(created.status).toBe(201);
  const initial = await created.json() as { sha256: string; result: string; size_bytes: number };
  expect(initial).toMatchObject({ result: 'created', size_bytes: firstBytes.length });
  const currentUrl = `${origin}/api/plugin-packages/document-tools/versions/1.0.0`;
  const current = await fetch(currentUrl, { headers: { authorization: `Bearer ${token}` } });
  expect(current.status).toBe(200);
  expect((await current.json() as { sha256: string }).sha256).toBe(initial.sha256);
  const repeated = await send(firstBytes);
  expect(repeated.status).toBe(200);
  expect((await repeated.json() as { result: string }).result).toBe('unchanged');

  // 替换会覆盖当前引用；先在独立文件中备份，再恢复到另一新库并精确核对关键表。
  const dump = join(backupRoot, 'before-replacement.dump');
  await run(join(pgBin, 'pg_dump'), ['-Fc', '-f', dump, url]);
  expect((await stat(dump)).size).toBeGreaterThan(0);
  const backedPackage = join(backupRoot, `${initial.sha256}.zip`);
  await copyFile(join(pluginRoot, 'packages', `${initial.sha256}.zip`), backedPackage);
  const backupBytes = await readFile(backedPackage);
  expect(backupBytes.length).toBe(initial.size_bytes);
  expect(createHash('sha256').update(backupBytes).digest('hex')).toBe(initial.sha256);
  const restoreName = `ez_center_c01_m0_restore_${Date.now()}`;
  expect((await admin.query('SELECT 1 FROM pg_database WHERE datname=$1', [restoreName])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${restoreName}" OWNER center_app`);
  const restoreUrl = new URL(url);
  restoreUrl.pathname = `/${restoreName}`;
  await run(join(pgBin, 'pg_restore'), ['--no-owner', '--no-privileges', '-d', restoreUrl.href, dump]);
  const restored = createPool({ url: restoreUrl.href });
  try {
    const expected = await counts();
    for (const [table, count] of Object.entries(expected)) {
      const row = await restored.query<{ count: string }>(`SELECT count(*) FROM public.${table}`);
      expect(row.rows[0]!.count).toBe(count);
    }
    const row = await restored.query<{ sha256: string }>(
      "SELECT sha256 FROM public.plugin_packages WHERE plugin_name='document-tools' AND version='1.0.0'");
    expect(row.rows[0]!.sha256).toBe(initial.sha256);
  } finally { await restored.end(); }

  const replaced = await send(await readFile(second));
  expect(replaced.status).toBe(200);
  const updated = await replaced.json() as { sha256: string; result: string };
  expect(updated.result).toBe('replaced');
  expect(updated.sha256).not.toBe(initial.sha256);
  const latest = await fetch(currentUrl, { headers: { authorization: `Bearer ${token}` } });
  expect((await latest.json() as { sha256: string }).sha256).toBe(updated.sha256);
  const malformed = await send(Buffer.from('PK\x03\x04invalid'));
  expect(malformed.status).toBe(400);
  expect((await malformed.json() as { error: { code: string } }).error.code).toBe('PACKAGE_INVALID');
  const retained = await fetch(currentUrl, { headers: { authorization: `Bearer ${token}` } });
  expect((await retained.json() as { sha256: string }).sha256).toBe(updated.sha256);
  expect((await pool.query<{ count: string }>('SELECT count(*) FROM public.plugin_packages')).rows[0]!.count).toBe('1');
  expect(await readdir(join(pluginRoot, 'staging'))).toEqual([]);
  expect((await readdir(join(pluginRoot, 'packages'))).sort()).toEqual(
    [`${initial.sha256}.zip`, `${updated.sha256}.zip`].sort());
  const audits = (await pool.query<{ reason_code: string | null; details: { result?: string } }>(
    "SELECT reason_code,details FROM public.management_audit WHERE action='plugin_package_uploaded' ORDER BY id")).rows;
  expect(audits.map(item => item.details.result).filter(Boolean)).toEqual(['created', 'unchanged', 'replaced']);
  expect(audits.some(item => item.reason_code === 'PACKAGE_INVALID')).toBe(true);

  const tarPath = join(backupRoot, 'complete.tar.gz');
  await tarArchive(tarPath);
  const tarBytes = await readFile(tarPath);
  const tarCreated = await send(tarBytes, token, 'application/gzip');
  expect(tarCreated.status).toBe(201);
  const tarResult = await tarCreated.json() as { result: string; archive_format: string; sha256: string };
  expect(tarResult).toMatchObject({ result: 'created', archive_format: 'tar_gzip' });
  expect((await fetch(`${origin}/api/plugin-packages/document-tools/versions/1.1.0`,
    { headers: { authorization: `Bearer ${token}` } })).status).toBe(200);
  expect(createHash('sha256').update(await readFile(join(pluginRoot, 'packages', `${tarResult.sha256}.tar.gz`)))
    .digest('hex')).toBe(tarResult.sha256);
  expect((await pool.query<{ count: string }>('SELECT count(*) FROM public.plugin_packages')).rows[0]!.count).toBe('2');

  const concurrentA = join(backupRoot, 'concurrent-a.zip');
  const concurrentB = join(backupRoot, 'concurrent-b.zip');
  await archive(concurrentA, 'Concurrent A');
  await archive(concurrentB, 'Concurrent B');
  const responses = await Promise.all([send(await readFile(concurrentA)), send(await readFile(concurrentB))]);
  expect(responses.map(response => response.status)).toEqual([200, 200]);
  expect((await Promise.all(responses.map(response => response.json())) as { result: string }[])
    .map(response => response.result)).toEqual(['replaced', 'replaced']);
  const latestAudit = (await pool.query<{ sha256: string }>(
    "SELECT details->>'sha256' AS sha256 FROM public.management_audit WHERE action='plugin_package_uploaded' AND details->>'result'='replaced' ORDER BY id DESC LIMIT 1"
  )).rows[0]!.sha256;
  const afterConcurrency = await fetch(currentUrl, { headers: { authorization: `Bearer ${token}` } });
  expect((await afterConcurrency.json() as { sha256: string }).sha256).toBe(latestAudit);
  expect((await pool.query<{ count: string }>('SELECT count(*) FROM public.plugin_packages')).rows[0]!.count).toBe('2');
});
