import { readFile, writeFile, mkdir, stat, readdir, access } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { runner } from 'node-pg-migrate';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { DesktopReleaseService } from '../../dist/desktop-releases/service.js';
import { parseManifest, verifyReleaseSignature } from '../../dist/desktop-releases/manifest.js';
import type { ReleaseManifest } from '../../dist/desktop-releases/manifest.js';

const root = resolve(process.env.CENTER_F01_SCENARIO ?? '');
if (!process.env.CENTER_F01_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f01-m0-'))) throw new Error('必须指定新建 F01 场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as { root: string; postgres_host: string; postgres_port: number; database: string; role: string; center_port: number };
if (scenario.root !== root || scenario.postgres_host !== '127.0.0.1' || scenario.database !== 'ez_center_f01_m0' || scenario.role !== 'center_app') throw new Error('隔离目标不符');
const config = { url: `postgres://center_app@127.0.0.1:${scenario.postgres_port}/${scenario.database}` };
const pool = createPool(config), source = createDataSource(config);
const origin = `http://127.0.0.1:${scenario.center_port}`;
const keyPath = join(root, 'test-signing.key');
const cli = resolve('../desktop/node_modules/@tauri-apps/cli/tauri.js');
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
const sha = (bytes: Uint8Array) => createHash('sha256').update(bytes).digest('hex');
const count = async (table: 'desktop_releases' | 'desktop_release_targets' | 'management_audit') => Number((await pool.query(`SELECT COUNT(*) FROM public.${table}`)).rows[0].count);

async function sign(path: string) {
  execFileSync(process.execPath, [cli, 'signer', 'sign', path], { env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PATH: keyPath, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: '' }, stdio: 'pipe' });
  return readFile(`${path}.sig`, 'utf8');
}

async function fixture(version: string, content: string, protocols = [3]) {
  const directory = join(root, 'fixtures', `${version}-${randomUUID()}`); await mkdir(directory, { recursive: true });
  const name = `EZ_${version}.msi`, bytes = Buffer.from(content);
  await writeFile(join(directory, name), bytes);
  const signature = await sign(join(directory, name));
  const manifest: ReleaseManifest = { product: 'ez-assistant', version, min_supported_version: '0.29.0', target: 'windows-x86_64',
    host: { version, min_supported_version: '0.29.0', enterprise_protocols: protocols },
    files: [{ name, purposes: ['installer', 'updater'], size: bytes.length, sha256: sha(bytes), signature }] };
  const text = JSON.stringify(manifest, null, 2) + '\n';
  await writeFile(join(directory, 'release.json'), text);
  return { manifest, text, signature: await sign(join(directory, 'release.json')), bytes, directory };
}

type Fixture = Awaited<ReturnType<typeof fixture>>;
function multipart(item: Fixture, extra?: { text?: string; bytes?: Buffer; signature?: string; extraFile?: boolean }) {
  const boundary = `f01-${randomUUID()}`;
  const parts: Buffer[] = [];
  for (const [name, value] of [['manifest', Buffer.from(extra?.text ?? item.text).toString('base64')], ['signature', extra?.signature ?? item.signature]]) {
    parts.push(Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="${name}"\r\n\r\n${value}\r\n`));
  }
  const file = (name: string, bytes: Buffer) => {
    parts.push(Buffer.from(`--${boundary}\r\nContent-Disposition: form-data; name="files"; filename="${name}"\r\nContent-Type: application/octet-stream\r\n\r\n`), bytes, Buffer.from('\r\n'));
  };
  file(item.manifest.files[0]!.name, extra?.bytes ?? item.bytes);
  if (extra?.extraFile) file('extra.msi', Buffer.from('extra'));
  parts.push(Buffer.from(`--${boundary}--\r\n`));
  return { contentType: `multipart/form-data; boundary=${boundary}`, bytes: Buffer.concat(parts) };
}

async function login(username = 'admin', password = '123456') {
  const result = await app!.inject({ method: 'POST', url: '/api/auth/login', payload: { username, password } });
  expect(result.statusCode).toBe(200); return result.json().token as string;
}

async function start() {
  app = await createHttpApp({ source, origin, desktopReleaseRoot: join(root, 'releases'), desktopUpdateKeyFile: `${keyPath}.pub` });
  await app.listen(scenario.center_port, '127.0.0.1');
}

afterAll(async () => { vi.restoreAllMocks(); await app?.close(); if (source.isInitialized) await source.destroy(); await pool.end(); });

it('签名、真实上传发布下载、权限、同版本覆盖、事务失败与撤回闭环', async () => {
  expect((await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0])
    .toEqual({ host: scenario.postgres_host, port: scenario.postgres_port, db: scenario.database, role: scenario.role, dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  try { await access(keyPath); } catch { execFileSync(process.execPath, [cli, 'signer', 'generate', '--ci', '--password', '', '--write-keys', keyPath], { stdio: 'pipe' }); }
  // 先保留旧版本身份数据，再按正式升级入口只追加本次迁移。
  const migrationClient = await pool.connect();
  try {
    await runner({ dbClient: migrationClient, dir: upgradeFiles().directory, direction: 'up', count: 6,
      migrationsTable: 'pgmigrations', schema: 'public', migrationsSchema: 'public',
      singleTransaction: true, verbose: false, logger: { info() {}, warn() {}, error() {} } });
  } finally { migrationClient.release(true); }
  const oldUsers = (await pool.query('SELECT * FROM public.users ORDER BY id')).rows;
  expect(oldUsers).toHaveLength(1);
  await upgradeDatabase(pool);
  expect((await pool.query('SELECT * FROM public.users ORDER BY id')).rows).toEqual(oldUsers);
  await source.initialize(); await start();
  let admin = await login();
  const fixtureA = await fixture('0.29.1', 'test installer A');
  await writeFile(join(root, 'browser-fixture.json'), JSON.stringify({ directory: fixtureA.directory, names: ['release.json', 'release.json.sig', fixtureA.manifest.files[0]!.name] }));
  const publicKey = await readFile(`${keyPath}.pub`, 'utf8');
  expect(() => verifyReleaseSignature(Buffer.from(fixtureA.text), fixtureA.signature, publicKey)).not.toThrow();
  expect(() => verifyReleaseSignature(Buffer.from(fixtureA.text + ' '), fixtureA.signature, publicKey)).toThrow();
  expect(() => parseManifest(JSON.stringify({ ...fixtureA.manifest, target: '../windows' }))).toThrow();
  const upload = async (item: Fixture, token = admin, extra?: Parameters<typeof multipart>[1]) => {
    const body = multipart(item, extra);
    const response = await fetch(`${origin}/api/desktop-releases/packages`, { method: 'POST', headers: { ...headers(token), 'content-type': body.contentType }, body: new Uint8Array(body.bytes) });
    const text = await response.text();
    return { statusCode: response.status, body: text, json: () => JSON.parse(text) };
  };
  expect((await upload(fixtureA, '')).statusCode).toBe(401);
  expect((await app!.inject({ method: 'POST', url: '/api/users', headers: headers(admin), payload: { username: 'f01-user', display_name: 'F01 test', role: 'user', enabled: true, password: 'test1234' } })).statusCode).toBe(201);
  const user = await login('f01-user', 'test1234');
  expect((await upload(fixtureA, user)).statusCode).toBe(403);
  expect((await upload(fixtureA, admin, { text: fixtureA.text.replace('0.29.1', '0.29.9') })).statusCode).toBe(400);
  expect((await upload(fixtureA, admin, { bytes: Buffer.from('broken') })).statusCode).toBe(400);
  expect((await upload(fixtureA, admin, { extraFile: true })).statusCode).toBe(400);
  expect(await count('desktop_release_targets')).toBe(0);
  const uploaded = await upload(fixtureA);
  expect(uploaded.statusCode, uploaded.body).toBe(200);
  expect(uploaded.json().result).toBe('created');
  expect((await upload(fixtureA)).json().result).toBe('unchanged');
  expect(await count('desktop_releases')).toBe(1); expect(await count('desktop_release_targets')).toBe(1);
  const fileOwner = app!.get(DesktopReleaseService).files, receive = fileOwner.receive.bind(fileOwner);
  let arrived = 0, releaseBarrier!: () => void;
  const barrier = new Promise<void>(resolve => { releaseBarrier = resolve; });
  const hold = vi.spyOn(fileOwner, 'receive').mockImplementation(async (...args) => {
    await receive(...args); if (++arrived === 2) releaseBarrier(); await barrier;
  });
  const replacements = await Promise.all([fixture('0.29.1', 'replacement B'), fixture('0.29.1', 'replacement C')]);
  const draftRace = await Promise.all(replacements.map(item => upload(item)));
  hold.mockRestore();
  expect(draftRace.map(result => result.statusCode).sort()).toEqual([200, 409]);
  expect((await upload(fixtureA)).json().result).toBe('replaced');
  const detail = () => app!.inject({ url: '/api/desktop-releases/0.29.1', headers: headers(admin) });
  const row = (await detail()).json().targets[0];
  expect(row.publication).toBe('draft');
  const check = '/api/desktop-updates/windows-x86_64/0.29.0';
  expect((await app!.inject(check)).statusCode).toBe(204);
  expect((await app!.inject('/api/desktop-updates/windows-x86_64/releases/0.29.1/files/updater')).statusCode).toBe(404);
  const publish = (publication: string, expected: string, sha256 = row.manifest_sha256) => app!.inject({ method: 'PUT', url: '/api/desktop-releases/0.29.1/targets/windows-x86_64/publication', headers: headers(admin), payload: { manifest_sha256: sha256, expected_publication: expected, publication } });
  expect((await publish('published', 'draft', '0'.repeat(64))).statusCode).toBe(409);
  const audit = vi.spyOn(app!.get(IdentityService), 'audit').mockRejectedValueOnce(new Error('test transaction rollback'));
  expect((await publish('published', 'draft')).statusCode).toBe(503);
  expect((await detail()).json().targets[0].publication).toBe('draft'); audit.mockRestore();
  expect((await publish('published', 'draft')).statusCode).toBe(200);
  const beforeNoOp = await count('management_audit');
  expect((await publish('published', 'draft')).statusCode).toBe(200); expect(await count('management_audit')).toBe(beforeNoOp);
  const candidate = (await app!.inject(check)).json();
  expect(candidate.version).toBe('0.29.1'); expect(candidate.manifest).toBe(fixtureA.text);
  expect((await app!.inject('/api/desktop-updates/windows-x86_64/0.29.1')).statusCode).toBe(204);
  expect((await app!.inject('/api/desktop-updates/darwin-aarch64/0.29.0')).statusCode).toBe(204);
  const response = await fetch(candidate.url, { redirect: 'error' });
  expect(response.status).toBe(200); expect(response.headers.get('cache-control')).toBe('no-store');
  expect(Buffer.from(await response.arrayBuffer())).toEqual(fixtureA.bytes);
  const publishedBefore = (await detail()).json().targets[0];
  const replacement = await fixture('0.29.1', 'different installer');
  // 发布中的替换仍须校验全部内容，事务失败保留旧清单与可下载文件。
  expect((await upload(replacement, admin, { bytes: Buffer.from('broken') })).statusCode).toBe(400);
  const failedReplace = vi.spyOn(app!.get(IdentityService), 'audit').mockRejectedValueOnce(new Error('test replacement rollback'));
  expect((await upload(replacement)).statusCode).toBe(503);
  failedReplace.mockRestore();
  expect((await detail()).json().targets[0]).toEqual(publishedBefore);
  expect(Buffer.from(await (await fetch(candidate.url)).arrayBuffer())).toEqual(fixtureA.bytes);
  expect((await upload(replacement)).json().result).toBe('replaced');
  const replaced = (await detail()).json().targets[0];
  expect(replaced.publication).toBe('published');
  expect(replaced.first_published_at).toBe(publishedBefore.first_published_at);
  expect(replaced.manifest_text).toBe(replacement.text);
  expect((await app!.inject(check)).json().manifest).toBe(replacement.text);
  expect(Buffer.from(await (await fetch(candidate.url)).arrayBuffer())).toEqual(replacement.bytes);
  expect((await app!.inject('/api/site/downloads')).json()[0].sha256).toBe(sha(replacement.bytes));
  expect((await publish('withdrawn', 'published')).statusCode).toBe(409); // 旧清单不能撤回新包。
  const replaceAudit = await count('management_audit');
  expect((await upload(replacement)).json().result).toBe('unchanged');
  expect(await count('management_audit')).toBe(replaceAudit);
  expect((await upload(fixtureA)).json().result).toBe('replaced');
  const notes = (notes: string, expected_notes: string) => app!.inject({ method: 'PUT', url: '/api/desktop-releases/0.29.1/notes', headers: headers(admin), payload: { notes, expected_notes } });
  const raced = await Promise.all([notes('更新内容 A', ''), notes('更新内容 B', '')]);
  expect(raced.map(value => value.statusCode).sort()).toEqual([200, 409]);
  expect((await app!.inject(check)).json().notes).toMatch(/^更新内容 /);
  expect((await app!.inject(`${check}?host_version=0.28.0&host_min_supported_version=0.28.0&host_protocols=2`)).statusCode).toBe(204);
  expect((await app!.inject(`${check}?host_version=0.29.0&host_min_supported_version=0.29.0&host_protocols=3`)).statusCode).toBe(200);
  const newer = await fixture('0.29.10', 'newer installer');
  expect((await upload(newer)).statusCode).toBe(200);
  expect((await app!.inject({ method: 'PUT', url: '/api/desktop-releases/0.29.10/targets/windows-x86_64/publication', headers: headers(admin), payload: { manifest_sha256: sha(Buffer.from(newer.text)), expected_publication: 'draft', publication: 'published' } })).statusCode).toBe(200);
  expect((await app!.inject(check)).json().version).toBe('0.29.10');
  const download = await app!.get(DesktopReleaseService).published('windows-x86_64', '0.29.1', 'updater');
  expect(download.stream!.destroyed).toBe(false);
  expect((await publish('withdrawn', 'published')).statusCode).toBe(200);
  expect(download.stream!.destroyed).toBe(true);
  expect((await fetch(candidate.url)).status).toBe(404);
  expect((await stat(join(root, 'releases', fixtureA.manifest.files[0]!.sha256))).size).toBe(fixtureA.bytes.length);
  const withdrawnReplacement = await fixture('0.29.1', 'replace withdrawn');
  expect((await upload(withdrawnReplacement)).json().result).toBe('replaced');
  const withdrawn = (await detail()).json().targets[0];
  expect(withdrawn.publication).toBe('withdrawn');
  expect(withdrawn.first_published_at).toBe(publishedBefore.first_published_at);
  expect(withdrawn.manifest_text).toBe(withdrawnReplacement.text);
  expect((await fetch(candidate.url)).status).toBe(404);
  expect((await upload(fixtureA)).json().result).toBe('replaced');
  const staleToken = admin;
  await app!.close(); await start(); admin = await login();
  expect((await app!.inject({ url: '/api/desktop-releases', headers: headers(staleToken) })).statusCode).toBe(401);
  expect((await detail()).json().targets[0].publication).toBe('withdrawn');
  expect((await publish('published', 'withdrawn')).statusCode).toBe(200);
  expect((await readdir(join(root, 'releases'))).filter(name => name.endsWith('.part'))).toEqual([]);
  expect(await count('desktop_releases')).toBe(2); expect(await count('desktop_release_targets')).toBe(2);
  const rows = (await pool.query('SELECT version,target,publication,manifest_sha256 FROM public.desktop_release_targets ORDER BY version')).rows;
  expect(rows.every(row => row.publication === 'published')).toBe(true);
  console.log(JSON.stringify({ target: { host: scenario.postgres_host, port: scenario.postgres_port, database: scenario.database }, tables: { desktop_releases: await count('desktop_releases'), desktop_release_targets: await count('desktop_release_targets'), management_audit: await count('management_audit') }, rows }));
}, 60000);
