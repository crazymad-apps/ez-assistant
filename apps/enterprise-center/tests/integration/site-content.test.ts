import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join, dirname } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { publishDirectory } from '../../dist/site-content/directory.js';
import { siteFixture } from '../support/site-content.js';

const root = resolve(process.env.CENTER_SITE_SCENARIO ?? '');
if (!process.env.CENTER_SITE_SCENARIO || !root.startsWith(resolve('../../.runtime-test/v0.31.0-f01-directory-'))) throw new Error('必须使用本次新建的一次性 F01 场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as { root: string; postgres_host: string; postgres_port: number; database: string; role: string; center_port: number };
if (scenario.root !== root || scenario.postgres_host !== '127.0.0.1' || scenario.database !== 'ez_f01_directory' || scenario.role !== 'center_app') throw new Error('隔离目标不符');
const config = { url: `postgres://center_app@127.0.0.1:${scenario.postgres_port}/${scenario.database}` };
const pool = createPool(config), source = createDataSource(config), origin = `http://127.0.0.1:${scenario.center_port}`;
const key = join(root, 'test.key'), cli = resolve('../desktop/node_modules/@tauri-apps/cli/tauri.js');
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
const hash = (bytes: Buffer | string) => createHash('sha256').update(bytes).digest('hex');
async function sign(path: string) { execFileSync(process.execPath, [cli, 'signer', 'sign', path], { env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PATH: key, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: '' }, stdio: 'pipe' }); return readFile(`${path}.sig`, 'utf8'); }
async function installer(version: string, target = 'windows-x86_64') {
  const directory = join(root, `${version}-${target}`); await mkdir(directory); const name = `EZ-${version}.msi`, bytes = Buffer.from('isolated test installer');
  await writeFile(join(directory, name), bytes); const signature = await sign(join(directory, name));
  const manifest = JSON.stringify({ product: 'ez-assistant', version, min_supported_version: '0.29.0', target, host: { version, min_supported_version: '0.29.0', enterprise_protocols: [3] }, files: [{ name, purposes: ['installer', 'updater'], size: bytes.length, sha256: hash(bytes), signature }] });
  await writeFile(join(directory, 'release.json'), manifest);
  const form = new FormData(); form.append('manifest', Buffer.from(manifest).toString('base64')); form.append('signature', await sign(join(directory, 'release.json'))); form.append('files', new Blob([bytes]), name);
  return { directory, form, manifest };
}
async function start() {
  app = await createHttpApp({ source, origin, desktopReleaseRoot: join(root, 'releases'), desktopUpdateKeyFile: `${key}.pub`, siteContentRoot: join(root, 'public-content') }, resolve('../enterprise-admin/dist/admin'));
  await app.listen(scenario.center_port, '127.0.0.1');
}
async function login(username = 'admin', password = '123456') {
  const response = await app!.inject({ method: 'POST', url: '/api/auth/login', payload: { username, password } });
  expect(response.statusCode).toBe(200); return response.json().token as string;
}
afterAll(async () => { vi.restoreAllMocks(); await app?.close(); if (source.isInitialized) await source.destroy(); await pool.end(); });

it('日志复用版本说明，静态手册独立于上传发布，失败与重启可恢复', async () => {
  // 首次写入前核对真实连接、数据目录和空库，不以环境变量名称替代隔离证明。
  const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0];
  expect(actual).toEqual({ host: scenario.postgres_host, port: scenario.postgres_port, db: scenario.database, role: scenario.role, dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  await upgradeDatabase(pool);
  expect((await pool.query("SELECT COUNT(*) FROM information_schema.columns WHERE table_name='desktop_releases' AND column_name='site_content_sha256'")).rows[0].count).toBe('0');
  execFileSync(process.execPath, [cli, 'signer', 'generate', '--ci', '--password', '', '--write-keys', key], { stdio: 'pipe' });
  await source.initialize(); await start(); let admin = await login();
  const get = (url: string, token?: string) => app!.inject({ url, headers: token ? headers(token) : {} });
  const notes = (v: string, text: string, expected: string, token = admin) => app!.inject({ method: 'PUT', url: `/api/desktop-releases/${v}/notes`, headers: token ? headers(token) : {}, payload: { notes: text, expected_notes: expected } });
  const publish = (v: string, manifest: string, publication = 'published', expected_publication = 'draft', target = 'windows-x86_64') => app!.inject({ method: 'PUT', url: `/api/desktop-releases/${v}/targets/${target}/publication`, headers: headers(admin), payload: { manifest_sha256: hash(manifest), publication, expected_publication } });
  expect((await get('/api/site/changelog')).json()).toEqual([]);
  const sourceDir = join(root, 'content-source');
  for (const [name, bytes] of siteFixture()) { await mkdir(dirname(join(sourceDir, name)), { recursive: true }); await writeFile(join(sourceDir, name), bytes); }
  await publishDirectory(sourceDir, join(root, 'public-content'), 'build');
  const before = (await get('/site-content/content.json')).body;
  expect(before).toContain('隔离验证'); // 没有安装版本也能读取手册。
  const release = await installer('0.31.0');
  expect((await fetch(`${origin}/api/desktop-releases/packages`, { method: 'POST', headers: headers(admin), body: release.form })).status).toBe(200);
  expect((await notes('0.31.0', '**初次发布**', '', '')).statusCode).toBe(401);
  expect((await notes('0.31.0', '**初次发布**', '')).statusCode).toBe(200);
  expect((await get('/api/site/changelog')).json()).toEqual([]);
  const preview = () => app!.inject({ method: 'POST', url: '/api/desktop-releases/notes-preview', headers: headers(admin), payload: { notes: '**初次发布**' } });
  expect((await preview()).json()[0].children.some((node: { kind: string }) => node.kind === 'strong')).toBe(true);
  expect((await app!.inject({ method: 'POST', url: '/api/desktop-releases/notes-preview', payload: { notes: 'draft' } })).statusCode).toBe(401);
  expect((await notes('0.31.0', '未核对的覆盖', '')).statusCode).toBe(409);
  const audit = vi.spyOn(app!.get(IdentityService), 'audit').mockRejectedValueOnce(new Error('isolated rollback'));
  expect((await notes('0.31.0', '不应保存', '**初次发布**')).statusCode).toBe(503); audit.mockRestore();
  expect((await get('/api/desktop-releases/0.31.0', admin)).json().notes).toBe('**初次发布**');
  expect((await publish('0.31.0', release.manifest)).statusCode).toBe(200);
  const arm = await installer('0.31.0', 'windows-aarch64');
  expect((await fetch(`${origin}/api/desktop-releases/packages`, { method: 'POST', headers: headers(admin), body: arm.form })).status).toBe(200);
  expect((await publish('0.31.0', arm.manifest, 'published', 'draft', 'windows-aarch64')).statusCode).toBe(200);
  const log = (await get('/api/site/changelog')).json(); expect(log).toHaveLength(1);
  expect(log[0].body).toEqual((await preview()).json());
  expect((await get('/site-content/content.json')).body).toBe(before);
  expect((await get('/api/desktop-updates/windows-x86_64/releases/0.31.0')).json().notes).toBe('**初次发布**');
  expect((await notes('0.31.0', '- 修正说明', '**初次发布**')).statusCode).toBe(200);
  expect((await get('/api/site/changelog')).json()[0].published_at).toBe(log[0].published_at);
  expect((await get('/api/desktop-updates/windows-x86_64/releases/0.31.0')).json().notes).toBe('- 修正说明');
  expect((await publish('0.31.0', release.manifest, 'withdrawn', 'published')).statusCode).toBe(200);
  expect((await publish('0.31.0', arm.manifest, 'withdrawn', 'published', 'windows-aarch64')).statusCode).toBe(200);
  expect((await get('/api/site/changelog')).json()).toHaveLength(1);
  expect((await get('/api/desktop-updates/windows-x86_64/releases/0.31.0/files/installer')).statusCode).toBe(404);
  expect((await get('/site-content/content.json')).body).toBe(before);
  await writeFile(join(sourceDir, 'home/hero.md'), siteFixture('0.31.0', '手册独立修订').get('home/hero.md')!);
  await publishDirectory(sourceDir, join(root, 'public-content'), 'publish');
  expect((await get('/site-content/content.json')).body).toContain('手册独立修订');
  expect((await get('/api/desktop-releases/0.31.0', admin)).json().notes).toBe('- 修正说明');
  // 拒绝普通成员编辑或预览管理员草稿。
  expect((await app!.inject({ method: 'POST', url: '/api/users', headers: headers(admin), payload: { username: 'site-user', display_name: 'Test', role: 'user', enabled: true, password: 'test1234' } })).statusCode).toBe(201);
  const user = await login('site-user', 'test1234');
  expect((await notes('0.31.0', 'no', '- 修正说明', user)).statusCode).toBe(403);
  expect((await app!.inject({ method: 'POST', url: '/api/desktop-releases/notes-preview', headers: headers(user), payload: { notes: 'draft' } })).statusCode).toBe(403);
  const newer = await installer('0.31.1');
  expect((await fetch(`${origin}/api/desktop-releases/packages`, { method: 'POST', headers: headers(admin), body: newer.form })).status).toBe(200);
  expect((await notes('0.31.1', '**下一版**', '')).statusCode).toBe(200);
  expect((await get('/api/site/changelog')).json()).toHaveLength(1);
  expect((await publish('0.31.1', newer.manifest)).statusCode).toBe(200);
  expect((await get('/api/site/changelog')).json().map((item: { version: string }) => item.version)).toEqual(['0.31.1', '0.31.0']);
  await app!.close(); await start(); admin = await login();
  expect((await get('/api/site/changelog')).json()).toHaveLength(2);
  expect((await get('/site-content/content.json')).body).toContain('手册独立修订');
  expect((await get('/api/desktop-releases/0.31.0/site-content/status', admin)).statusCode).toBe(404);
  const tables = (await pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")).rows;
  const counts: Record<string, number> = {};
  for (const { tablename } of tables) counts[tablename] = Number((await pool.query(`SELECT COUNT(*) FROM public."${tablename}"`)).rows[0].count);
  expect(counts.desktop_releases).toBe(2); expect(counts.desktop_release_targets).toBe(3); expect(counts.users).toBe(2);
  console.log(JSON.stringify({ actual, counts, releases: (await pool.query('SELECT version,notes FROM public.desktop_releases ORDER BY version')).rows }));
}, 60000);
