import { randomUUID, scrypt } from 'node:crypto';
import { cpSync, mkdtempSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { afterAll, beforeAll, expect, it, vi } from 'vitest';
import { PG_MIGRATE_LOCK_ID } from 'node-pg-migrate';
import { createPool } from '../../dist/database/pool.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { checkDatabase } from '../../dist/database/check.js';
import { checkDatabaseVersion, isUninitializedDatabase } from '../../dist/database/admission.js';
import { counts } from '../../dist/database/counts.js';
import * as migrations from '../../dist/database/migrations.js';
import { startService } from '../../dist/service.js';
import { loadConfig } from '../../dist/config.js';

// 只有测试建库操作使用 postgres；被测初始化、升级和业务连接始终使用同一非超级用户 center_app。
const url = process.env.CENTER_TEST_DATABASE_URL;
const adminUrl = process.env.CENTER_TEST_ADMIN_URL;
const restoreDatabase = process.env.CENTER_TEST_RESTORE_DATABASE;
if (!url || !adminUrl || !restoreDatabase) throw new Error('须显式提供隔离应用连接、测试建库连接和已验证恢复库');
const target = new URL(url);
const adminTarget = new URL(adminUrl);
if (target.hostname !== '127.0.0.1' || target.port !== '55432' || target.username !== 'center_app'
  || !/^\/ez_center_c01_test_[a-z0-9_]+$/.test(target.pathname) || adminTarget.host !== target.host
  || adminTarget.username !== 'postgres' || adminTarget.pathname !== '/postgres'
  || !/^ez_center_c01_restore_[a-z0-9_]+$/.test(restoreDatabase)) throw new Error('隔离目标不符合测试约束');
if (process.env.CENTER_TEST_BASELINE_BACKUP_VERIFIED !== 'true') throw new Error('须先独立备份并实际恢复核验');
const app = createPool({ url });
const admin = createPool({ url: adminUrl });
const extraPools: ReturnType<typeof createPool>[] = [];
const baselineCounts = { center_identity: '1', management_audit: '1', pgmigrations: '1', users: '1' };

async function snapshot(pool: ReturnType<typeof createPool>) {
  return {
    users: (await pool.query('SELECT * FROM public.users ORDER BY id')).rows,
    identity: (await pool.query('SELECT * FROM public.center_identity')).rows,
    audit: (await pool.query('SELECT * FROM public.management_audit ORDER BY id')).rows,
    ledger: (await pool.query('SELECT * FROM public.pgmigrations ORDER BY run_on,id')).rows,
  };
}

beforeAll(async () => {
  console.log({ host: target.hostname, port: target.port, database: target.pathname.slice(1), role: target.username });
  expect((await checkDatabase(app, true)).counts).toEqual(baselineCounts);
  const uri = new URL(url); uri.pathname = '/' + restoreDatabase;
  const restored = createPool({ url: uri.href });
  try {
    expect((await checkDatabase(restored, true)).counts).toEqual(baselineCounts);
    expect(await snapshot(restored)).toEqual(await snapshot(app));
  } finally { await restored.end(); }
});
afterAll(async () => {
  try { console.log({ final: await checkDatabase(app, true) }); }
  finally { await Promise.all([app.end(), admin.end(), ...extraPools.map(pool => pool.end())]); }
});

async function fixture(copyBackup = false) {
  const name = `ez_center_c01_test_${randomUUID().replaceAll('-', '')}`;
  expect((await admin.query('SELECT datname FROM pg_database WHERE datname=$1', [name])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${name}" OWNER center_app${copyBackup ? ` TEMPLATE "${restoreDatabase}"` : ''}`);
  const uri = new URL(url!); uri.pathname = '/' + name;
  const pool = createPool({ url: uri.href }); extraPools.push(pool);
  if (copyBackup) expect(await snapshot(pool)).toEqual(await snapshot(app));
  else {
    const c = await pool.connect();
    try { expect(await isUninitializedDatabase(c)).toBe(true); } finally { c.release(); }
  }
  console.log({ fixture_database: name, baseline: copyBackup ? restoreDatabase : 'empty' });
  return { pool, uri };
}

async function freePort() {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('port missing');
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}
function envFor(uri: string, port: number, autoUpgrade = 'true') {
  return { CENTER_DATABASE_URL: uri, CENTER_DATABASE_AUTO_UPGRADE: autoUpgrade, CENTER_PORT: String(port),
    CENTER_HOST: '127.0.0.1', CENTER_PUBLIC_ORIGIN: `http://127.0.0.1:${port}`, CENTER_ALLOW_HTTP_LOOPBACK: 'true' };
}
function testUpgradeFiles(kind: 'success' | 'failure' | 'initial-failure') {
  const base = migrations.upgradeFiles();
  const directory = mkdtempSync(join(tmpdir(), 'ez-center-upgrade-sql-'));
  cpSync(base.directory, directory, { recursive: true });
  cpSync(`tests/fixtures/upgrades/${kind}`, directory, { recursive: true });
  const names = readdirSync(directory).sort().map(file => file.slice(0, -4));
  // 只在测试模块替换文件发现；框架真实执行磁盘 SQL，生产没有自定义目录/SQL 注入配置。
  return vi.spyOn(migrations, 'upgradeFiles').mockReturnValue({ directory, names });
}
async function exactCounts(pool: ReturnType<typeof createPool>) {
  const client = await pool.connect();
  try { return await counts(client); } finally { client.release(); }
}

it('同一非超级用户具有本库所有权，重复升级不重跑、不改数据', async () => {
  expect((await app.query("SELECT rolsuper,rolcreatedb,rolcreaterole FROM pg_roles WHERE rolname=current_user")).rows)
    .toEqual([{ rolsuper: false, rolcreatedb: false, rolcreaterole: false }]);
  const before = await snapshot(app);
  expect(await upgradeDatabase(app)).toEqual({ applied: [] });
  expect(await snapshot(app)).toEqual(before);
});

it('关闭自动升级，已初始化库可启动且不修改数据', async () => {
  const before = await snapshot(app);
  const port = await freePort();
  const service = await startService(loadConfig(envFor(url!, port, 'false')));
  try { expect((await fetch(`${service.address}/api/info`)).status).toBe(200); }
  finally { await Promise.all([service.close(), service.close()]); }
  expect(await snapshot(app)).toEqual(before);
});

it('关闭自动升级，空库拒绝启动且不创建任何表', async () => {
  const f = await fixture(); const port = await freePort();
  await expect(startService(loadConfig(envFor(f.uri.href, port, 'false')))).rejects.toMatchObject({ code: 'UPGRADE_REQUIRED' });
  expect((await f.pool.query("SELECT count(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await expect(fetch(`http://127.0.0.1:${port}/api/info`)).rejects.toThrow();
});

it('默认自动升级空库并初始化超级管理员，之后才开放 HTTP', async () => {
  const f = await fixture(); const port = await freePort();
  const env = envFor(f.uri.href, port);
  const service = await startService(loadConfig({ ...env, CENTER_DATABASE_AUTO_UPGRADE: undefined }));
  try {
    expect(await (await fetch(`${service.address}/api/info`)).json()).toEqual({ software_version: '0.1.0', protocol_version: 1, min_protocol_version: 1 });
    expect(await exactCounts(f.pool)).toEqual(baselineCounts);
    const user = (await f.pool.query('SELECT username,role,is_super_admin,enabled,password_hash FROM public.users')).rows[0];
    expect(user).toMatchObject({ username: 'admin', role: 'admin', is_super_admin: true, enabled: true });
    const hash = String(user.password_hash);
    expect(hash).toMatch(/^scrypt\$131072\$8\$1\$[a-f0-9]{32}\$[a-f0-9]{64}$/);
    const parts = hash.split('$');
    const derived = await new Promise<Buffer>((resolve, reject) => scrypt('123456', Buffer.from(parts[4]!, 'hex'), 32,
      { N: 131072, r: 8, p: 1, maxmem: 192 * 1024 * 1024 }, (error, key) => error ? reject(error) : resolve(key)));
    expect(derived.toString('hex')).toBe(parts[5]);
  } finally { await service.close(); }
});

it('初始化 SQL 不重跑，修改后的管理员资料不会被重启覆盖', async () => {
  const f = await fixture(true);
  await f.pool.query("UPDATE public.users SET display_name='已修改的管理员', password_hash='test-updated-hash' WHERE username='admin'");
  const before = await snapshot(f.pool);
  const service = await startService(loadConfig(envFor(f.uri.href, await freePort())));
  await service.close();
  expect(await snapshot(f.pool)).toEqual(before);
  expect(await exactCounts(f.pool)).toEqual(baselineCounts);
});

it('关闭开关时旧版本拒绝启动，开启后按序升级并保留超级管理员', async () => {
  const f = await fixture(true); const before = await snapshot(f.pool);
  const mock = testUpgradeFiles('success');
  try {
    const port = await freePort();
    await expect(startService(loadConfig(envFor(f.uri.href, port, 'false')))).rejects.toMatchObject({ code: 'UPGRADE_REQUIRED' });
    expect(await snapshot(f.pool)).toEqual(before);
    const service = await startService(loadConfig(envFor(f.uri.href, port)));
    try {
      expect((await fetch(`${service.address}/api/info`)).status).toBe(200);
      expect(await upgradeDatabase(f.pool)).toEqual({ applied: [] });
      expect(await exactCounts(f.pool)).toEqual({ ...baselineCounts, pgmigrations: '3' });
      expect((await snapshot(f.pool)).users).toEqual(before.users.map(user => ({ ...user, upgrade_note: 'after' })));
    } finally { await service.close(); }
  } finally { mock.mockRestore(); }
  await expect(upgradeDatabase(f.pool)).rejects.toMatchObject({ code: 'SCHEMA_MISMATCH' });
});

it('中途升级失败整体回滚业务 SQL 和记录，后续 SQL 不执行且不监听', async () => {
  const f = await fixture(true); const before = await snapshot(f.pool);
  const mock = testUpgradeFiles('failure');
  try {
    const port = await freePort();
    await expect(startService(loadConfig(envFor(f.uri.href, port)))).rejects.toMatchObject({ code: '22012' });
    expect(await snapshot(f.pool)).toEqual(before);
    expect(await exactCounts(f.pool)).toEqual(baselineCounts);
    expect((await f.pool.query("SELECT to_regclass('public.must_not_run') AS relation")).rows[0].relation).toBeNull();
    await expect(fetch(`http://127.0.0.1:${port}/api/info`)).rejects.toThrow();
  } finally { mock.mockRestore(); }
});

it('首次初始化后有 SQL 失败，建表、超级管理员和审计全部回滚', async () => {
  const f = await fixture(); const mock = testUpgradeFiles('initial-failure');
  try { await expect(upgradeDatabase(f.pool)).rejects.toMatchObject({ code: '22012' }); }
  finally { mock.mockRestore(); }
  expect((await f.pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public'")).rows).toEqual([{ tablename: 'pgmigrations' }]);
  expect((await f.pool.query('SELECT count(*) FROM public.pgmigrations')).rows[0].count).toBe('0');
});

it('框架升级锁被占用时失败，不改数据库', async () => {
  const f = await fixture(true); const before = await snapshot(f.pool);
  const lock = await f.pool.connect();
  try {
    await lock.query('SELECT pg_advisory_lock($1)', [PG_MIGRATE_LOCK_ID]);
    await expect(upgradeDatabase(f.pool)).rejects.toThrow();
    expect(await snapshot(f.pool)).toEqual(before);
  } finally { await lock.query('SELECT pg_advisory_unlock($1)', [PG_MIGRATE_LOCK_ID]); lock.release(); }
});

it.each(["UPDATE public.pgmigrations SET name='999-unknown'", 'DELETE FROM public.pgmigrations'])('未知/缺失记录拒绝：%s', async sql => {
  const client = await app.connect();
  try {
    await client.query('BEGIN'); await client.query(sql);
    await expect(checkDatabaseVersion(client)).rejects.toMatchObject({ code: 'SCHEMA_MISMATCH' });
  } finally { await client.query('ROLLBACK'); client.release(); }
  expect((await checkDatabase(app, true)).counts).toEqual(baselineCounts);
});

it('不逐字段比对结构', async () => {
  const client = await app.connect();
  try { await client.query('BEGIN'); await client.query('ALTER TABLE public.users ALTER COLUMN enabled SET DEFAULT false'); expect(await checkDatabaseVersion(client)).toBe(1); }
  finally { await client.query('ROLLBACK'); client.release(); }
});

it('未知非空库拒绝认领且不建立框架记录表', async () => {
  const f = await fixture(); await f.pool.query('CREATE TABLE public.foreign_data(id integer)');
  const port = await freePort();
  await expect(startService(loadConfig(envFor(f.uri.href, port)))).rejects.toMatchObject({ code: 'SCHEMA_MISMATCH' });
  expect((await f.pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public'")).rows).toEqual([{ tablename: 'foreign_data' }]);
  expect((await f.pool.query('SELECT count(*) FROM public.foreign_data')).rows[0].count).toBe('0');
});

it('数据库不可用时进程退出，不输出连接秘密或监听成功', async () => {
  const port = await freePort(); const unavailable = new URL(url!); unavailable.port = String(await freePort());
  const child = spawn(process.execPath, ['dist/main.js', '--api-only'], { env: { ...process.env, ...envFor(unavailable.href, port) }, stdio: ['ignore','pipe','pipe'] });
  let output = ''; child.stdout.on('data', chunk => { output += String(chunk); }); child.stderr.on('data', chunk => { output += String(chunk); });
  expect(await new Promise<number | null>((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); })).toBe(1);
  expect(output).toContain('SERVICE_UNAVAILABLE'); expect(output).not.toContain(decodeURIComponent(unavailable.password));
  expect(output).not.toContain('center_listening');
});

it('真实进程 SIGTERM 后退出并释放监听', async () => {
  const port = await freePort();
  const child = spawn(process.execPath, ['dist/main.js', '--api-only'], { env: { ...process.env, ...envFor(url!, port) }, stdio: ['ignore','pipe','pipe'] });
  const exited = new Promise<number | null>((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  try {
    await new Promise<void>((resolve, reject) => {
      const deadline = setTimeout(() => reject(new Error('启动超时')), 3000);
      child.once('error', error => { clearTimeout(deadline); reject(error); });
      child.stdout.on('data', chunk => { if (String(chunk).includes('center_listening')) { clearTimeout(deadline); resolve(); } });
    });
    expect((await fetch(`http://127.0.0.1:${port}/api/info`)).status).toBe(200);
    child.kill('SIGTERM'); expect(await exited).toBe(0);
    await expect(fetch(`http://127.0.0.1:${port}/api/info`)).rejects.toThrow();
  } finally { if (child.exitCode === null) child.kill('SIGTERM'); }
});
