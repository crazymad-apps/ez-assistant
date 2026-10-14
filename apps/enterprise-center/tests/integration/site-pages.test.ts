import { readFile, writeFile, mkdir, rename } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { expect, it } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { startService } from '../../dist/service.js';
import { loadConfig } from '../../dist/config.js';

it('各平台独立选择最新可用 installer，指定版本不替换，发布与文件状态实时生效', async () => {
  const root = resolve(process.env.CENTER_SITE_SCENARIO ?? '');
  if (!process.env.CENTER_SITE_SCENARIO || !root.startsWith(resolve('../../.runtime-test/v0.31.0-f01-pages-'))) throw new Error('必须新建 F01 M1 隔离场景');
  const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8'));
  expect(scenario.root).toBe(root); expect(scenario.database).toBe('ez_f01_pages'); expect(scenario.role).toBe('center_app'); expect(scenario.postgres_host).toBe('127.0.0.1');
  const url = `postgres://center_app@127.0.0.1:${scenario.postgres_port}/ez_f01_pages`;
  const pool = createPool({ url });
  const origin = `http://127.0.0.1:${scenario.center_port}`;
  const key = join(root, 'test.key');
  const cli = resolve('../desktop/node_modules/@tauri-apps/cli/tauri.js');
  let service: Awaited<ReturnType<typeof startService>> | undefined;
  const hash = (bytes: Buffer | string) => createHash('sha256').update(bytes).digest('hex');
  try {
    const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0];
    expect(actual).toEqual({ host: '127.0.0.1', port: scenario.postgres_port, db: 'ez_f01_pages', role: 'center_app', dir: join(root, 'pgdata') });
    expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
    await upgradeDatabase(pool);
    execFileSync(process.execPath, [cli, 'signer', 'generate', '--ci', '--password', '', '--write-keys', key], { stdio: 'pipe' });
    const config = loadConfig({ CENTER_DATABASE_URL: url, CENTER_PUBLIC_ORIGIN: origin, CENTER_HOST: '127.0.0.1', CENTER_PORT: String(scenario.center_port), CENTER_DESKTOP_RELEASE_ROOT: join(root, 'releases'), CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE: `${key}.pub`, CENTER_SITE_CONTENT_ROOT: join(root, 'public-content') });
    service = await startService(config, resolve('../enterprise-admin/dist/admin'));
    const login = await fetch(`${origin}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: 'admin', password: '123456' }) });
    const token = (await login.json()).token;
    const headers = { authorization: `Bearer ${token}` };
    const catalog = async (suffix = '') => { const response = await fetch(`${origin}/api/site/downloads${suffix}`); expect(response.status).toBe(200); expect(response.headers.get('cache-control')).toBe('no-store'); return response.json(); };
    async function sign(path: string) {
      execFileSync(process.execPath, [cli, 'signer', 'sign', path], { env: { ...process.env, TAURI_SIGNING_PRIVATE_KEY_PATH: key, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: '' }, stdio: 'pipe' });
      return readFile(`${path}.sig`, 'utf8');
    }
    async function upload(version: string, target: string, protocols = [3], publish = true) {
      const directory = join(root, `${version}-${target}`); await mkdir(directory);
      const mac = target === 'darwin-aarch64';
      const names = mac ? [`EZ-${version}.dmg`, `EZ-${version}.app.tar.gz`] : [`EZ-${version}.msi`];
      const files = [];
      const form = new FormData();
      for (const [index, name] of names.entries()) {
        const bytes = Buffer.from(`isolated fixture ${target} ${version} ${index}`);
        await writeFile(join(directory, name), bytes);
        files.push({ name, purposes: mac ? [index === 0 ? 'installer' : 'updater'] : ['installer', 'updater'], size: bytes.length, sha256: hash(bytes), ...(!mac || index === 1 ? { signature: await sign(join(directory, name)) } : {}) });
      }
      const manifest = JSON.stringify({ product: 'ez-assistant', version, min_supported_version: '0.29.0', target, host: { version, min_supported_version: '0.29.0', enterprise_protocols: protocols }, files });
      await writeFile(join(directory, 'release.json'), manifest);
      form.append('manifest', Buffer.from(manifest).toString('base64')); form.append('signature', await sign(join(directory, 'release.json')));
      for (const name of names) form.append('files', new Blob([new Uint8Array(await readFile(join(directory, name)))]), name);
      expect((await fetch(`${origin}/api/desktop-releases/packages`, { method: 'POST', headers, body: form })).status).toBe(200);
      const publication = async (next = 'published', previous = 'draft') => {
        const response = await fetch(`${origin}/api/desktop-releases/${version}/targets/${target}/publication`, { method: 'PUT', headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ publication: next, expected_publication: previous, manifest_sha256: hash(manifest) }) });
        expect(response.status).toBe(200);
      };
      if (publish) await publication();
      return { files, publication };
    }
    expect(await catalog()).toEqual([]);
    await upload('0.31.8', 'windows-x86_64');
    const latest = await upload('0.31.10', 'windows-x86_64');
    await upload('0.31.9', 'windows-x86_64', [3], false); // 草稿不进入目录。
    await upload('0.31.11', 'windows-x86_64', [99]); // 协议不兼容。
    await upload('0.31.0', 'windows-aarch64');
    const mac = await upload('0.31.1', 'darwin-aarch64');
    const downloads = await catalog(); expect(downloads).toHaveLength(3);
    expect(downloads.find((row: { target: string }) => row.target === 'windows-x86_64').version).toBe('0.31.10');
    expect(downloads.find((row: { target: string }) => row.target === 'darwin-aarch64').name).toMatch(/\.dmg$/);
    for (const row of downloads) {
      expect(row.url).toMatch(/\/files\/installer$/);
      const response = await fetch(`${origin}${row.url}`);
      expect(response.status).toBe(200); expect(response.headers.get('content-disposition')).toContain(row.name);
      expect(hash(Buffer.from(await response.arrayBuffer()))).toBe(row.sha256);
    }
    expect(await catalog('?version=0.31.9')).toEqual([]);
    expect(await catalog('?version=0.31.11')).toEqual([]);
    expect(await catalog('?version=0.30.0')).toEqual([]);
    expect((await catalog('?version=0.31.1'))[0].name).toMatch(/\.dmg$/);
    expect((await fetch(`${origin}/api/site/downloads?version=invalid`)).status).toBe(400);
    expect((await fetch(`${origin}/api/site/downloads?unexpected=1`)).status).toBe(400);
    // 缺实际文件时回退更早可用版本，指定版本绝不静默换包。
    const sha = latest.files[0]!.sha256;
    const file = join(root, 'releases', sha);
    await rename(file, `${file}.isolated-missing`);
    expect((await catalog()).find((row: { target: string }) => row.target === 'windows-x86_64').version).toBe('0.31.8');
    expect(await catalog('?version=0.31.10')).toEqual([]);
    await rename(`${file}.isolated-missing`, file);
    await latest.publication('withdrawn', 'published');
    expect((await catalog()).find((row: { target: string }) => row.target === 'windows-x86_64').version).toBe('0.31.8');
    expect((await fetch(`${origin}/api/desktop-updates/windows-x86_64/releases/0.31.10/files/installer`)).status).toBe(404);
    // 日志来自版本 notes，静态内容缺失不影响下载。
    expect((await fetch(`${origin}/site-content/content.json`)).status).toBe(404);
    expect(mac.files).toHaveLength(2);
    const tables = (await pool.query("SELECT tablename FROM pg_tables WHERE schemaname='public' ORDER BY tablename")).rows;
    for (const { tablename } of tables) {
      if (!/^[a-z_]+$/.test(tablename)) throw new Error('Unexpected table');
      const count = (await pool.query(`SELECT COUNT(*) AS count FROM "${tablename}"`)).rows[0].count;
      process.stdout.write(`${tablename}: ${count}\n`);
    }
    expect((await pool.query('SELECT COUNT(*) FROM desktop_releases')).rows[0].count).toBe('6');
    expect((await pool.query('SELECT COUNT(*) FROM desktop_release_targets')).rows[0].count).toBe('6');
  } finally { await service?.close(); await pool.end(); }
}, 120000);
