import { createHash } from 'node:crypto';
import { execFile } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { copyFile, mkdir, readFile, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { promisify } from 'node:util';
import { afterAll, beforeAll, expect, it } from 'vitest';
import { ZipFile } from 'yazl';
import { createHttpApp } from '../../dist/app.js';
import { checkDatabase } from '../../dist/database/check.js';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';

const url = process.env.CENTER_TEST_AGENT_DATABASE_URL;
const adminUrl = process.env.CENTER_TEST_ADMIN_URL;
const pluginRoot = process.env.CENTER_TEST_PLUGIN_ROOT;
const backupRoot = process.env.CENTER_TEST_BACKUP_ROOT;
const pgBin = process.env.CENTER_TEST_PG_BIN;
if (!url || !adminUrl || !pluginRoot || !backupRoot || !pgBin || process.env.CENTER_TEST_ALLOW_WRITE !== 'true')
  throw new Error('须显式指定已核实的 C01 M2 新隔离库、包根、备份根及写入授权');
const target = new URL(url), adminTarget = new URL(adminUrl);
const scenario = resolve('../..', '.runtime-test');
if (target.hostname !== '127.0.0.1' || target.username !== 'center_app' ||
    !/^\/ez_center_c01_m2_[a-z0-9]+$/.test(target.pathname) ||
    adminTarget.hostname !== target.hostname || adminTarget.port !== target.port ||
    adminTarget.pathname !== '/postgres' || adminTarget.username !== 'postgres' ||
    !resolve(pluginRoot).startsWith(`${scenario}/c01-m2-`) ||
    !resolve(backupRoot).startsWith(`${scenario}/c01-m2-`))
  throw new Error('目标不是独立 C01 M2 场景');

const run = promisify(execFile);
const pool = createPool({ url });
const admin = createPool({ url: adminUrl });
const source = createDataSource({ url });
let app: Awaited<ReturnType<typeof createHttpApp>>;
let origin: string;

async function archive(path: string, skill: string) {
  const zip = new ZipFile();
  zip.addBuffer(Buffer.from(JSON.stringify({
    format_version: 1, name: 'document-tools', display_name: '文档工具', description: '读取文档',
    version: '1.0.0', platforms: [{ os: 'macos', arch: 'arm64' }], min_host_version: '0.28.0',
    skills: [skill], mcp: [], configuration: [], external_software: [],
  })), 'plugin.json');
  zip.addBuffer(Buffer.from(`---\nname: ${skill}\ndescription: Review documents\n---\n`),
    `skills/${skill}/SKILL.md`);
  zip.end();
  await pipeline(zip.outputStream, createWriteStream(path));
}

async function count(table: string): Promise<string> {
  return (await pool.query<{ count: string }>(`SELECT count(*) FROM public.${table}`)).rows[0]!.count;
}

beforeAll(async () => {
  const current = await pool.query(`SELECT current_user AS role,current_database() AS database,
    host(inet_server_addr()) AS host,inet_server_port() AS port`);
  expect(current.rows[0]).toEqual({ role: 'center_app', database: target.pathname.slice(1),
    host: target.hostname, port: Number(target.port) });
  expect((await checkDatabase(pool)).schema_version).toBe(3);
  expect(await count('custom_agents')).toBe('0');
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

it('完整 Agent 配置、即时依赖诊断、条件写、删除与独立恢复形成真实 HTTP 闭环', async () => {
  const request = (path: string, token: string, options: RequestInit = {}) =>
    fetch(`${origin}/api${path}`, { ...options, headers: { authorization: `Bearer ${token}`,
      ...options.headers } });
  const login = async (username: string, password: string) => {
    const response = await fetch(`${origin}/api/auth/login`, { method: 'POST',
      headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username, password }) });
    expect(response.status).toBe(200);
    return (await response.json() as { token: string }).token;
  };
  const adminToken = await login('admin', '123456');
  expect((await request('/runtime/agents', adminToken)).status).toBe(200);
  expect((await request('/agents', adminToken)).status).toBe(200);
  expect((await fetch(`${origin}/api/runtime/agents`)).status).toBe(401);
  const createdUser = await request('/users', adminToken, { method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'reader', display_name: '读取者', role: 'user',
      enabled: true, password: 'reader-password-123' }) });
  expect(createdUser.status).toBe(201);
  const userToken = await login('reader', 'reader-password-123');

  const write = (path: string, token: string, body: unknown, method = 'POST', ifMatch?: string) =>
    request(path, token, { method, headers: { 'content-type': 'application/json',
      ...(ifMatch ? { 'if-match': ifMatch } : {}) }, body: JSON.stringify(body) });
  const longPrompt = '中'.repeat(87381);
  const pure = { name: '纯提示词助手', description: '', prompt: longPrompt, skills: [], mcp: [] };
  const pureCreated = await write('/agents', adminToken, pure);
  expect(pureCreated.status).toBe(201);
  expect(pureCreated.headers.get('location')).toBe(`/api/agents/${encodeURIComponent(pure.name)}`);
  expect((await pureCreated.json() as { prompt: string }).prompt).toBe(longPrompt);
  expect((await write('/agents', userToken, pure)).status).toBe(403);
  expect((await write('/agents', adminToken, { ...pure, name: '过长助手', prompt: longPrompt + '中' })).status)
    .toBe(400);
  const unknown = { name: '文档审阅助手', description: '团队文档', prompt: '完整审阅。',
    skills: [{ name: 'document-review', plugin: 'document-tools' }],
    mcp: [{ name: 'local.server' }] };
  expect((await write('/agents', adminToken, unknown)).status).toBe(409);

  const first = join(backupRoot, 'agent-first.zip'), second = join(backupRoot, 'agent-second.zip');
  await archive(first, 'document-review');
  await archive(second, 'replacement-review');
  const upload = (bytes: Buffer) => request('/plugin-packages', adminToken, { method: 'POST',
    headers: { 'content-type': 'application/zip' }, body: new Uint8Array(bytes) });
  const firstBytes = await readFile(first);
  const packageCreated = await upload(firstBytes);
  expect(packageCreated.status).toBe(201);
  const originalSha = (await packageCreated.json() as { sha256: string }).sha256;
  const created = await write('/agents', adminToken, unknown);
  expect(created.status).toBe(201);
  expect(created.headers.get('etag')).toMatch(/^"[0-9a-f]{64}"$/);
  expect((await request('/agents', userToken)).status).toBe(403);
  const detailUrl = `/agents/${encodeURIComponent(unknown.name)}`;
  const unpublished = await request(detailUrl, adminToken);
  expect((await unpublished.json() as { diagnostics: { status: string }[] }).diagnostics[0]!.status)
    .toBe('plugin_unpublished');
  const pluginDetail = await request('/plugins/document-tools', adminToken);
  expect((await pluginDetail.json() as { dependent_agents: string[] }).dependent_agents)
    .toEqual([unknown.name]);

  const pluginEtag = pluginDetail.headers.get('etag')!;
  const listed = await write('/plugins/document-tools/publication', adminToken, { listed: true }, 'PUT', pluginEtag);
  expect(listed.status).toBe(200);
  const available = await request(detailUrl, adminToken);
  expect((await available.json() as { diagnostics: { status: string; platforms: unknown[] }[] }).diagnostics)
    .toMatchObject([{ status: 'available', platforms: [{ os: 'macos', arch: 'arm64' }] },
      { status: 'host_check_required', platforms: [] }]);
  const oldEtag = available.headers.get('etag')!;
  const runtime = await request('/runtime/agents', userToken);
  expect(runtime.status).toBe(200);
  const configurations = await runtime.json() as { name: string; prompt: string }[];
  expect(configurations.find(item => item.name === pure.name)?.prompt).toBe(longPrompt);
  expect(configurations.find(item => item.name === unknown.name)?.prompt).toBe(unknown.prompt);
  const summaries = await (await request('/agents', adminToken)).text();
  expect(summaries).not.toContain(longPrompt.slice(0, 100));

  const edited = await write(detailUrl, adminToken, { ...unknown, description: '更新说明' }, 'PUT', oldEtag);
  expect(edited.status).toBe(200);
  expect((await write(detailUrl, adminToken, { ...unknown, description: '旧表单' }, 'PUT', oldEtag)).status)
    .toBe(412);
  expect((await write(detailUrl, adminToken, unknown, 'PUT')).status).toBe(428);

  // 删除和同版本替换前，独立备份数据库及原包，并恢复到另一新库核对配置与各表数量。
  const dump = join(backupRoot, 'before-agent-replacement-and-delete.dump');
  await run(join(pgBin, 'pg_dump'), ['-Fc', '-f', dump, url]);
  expect((await stat(dump)).size).toBeGreaterThan(0);
  const packageBackup = join(backupRoot, `${originalSha}.zip`);
  await copyFile(join(pluginRoot, 'packages', `${originalSha}.zip`), packageBackup);
  expect(createHash('sha256').update(await readFile(packageBackup)).digest('hex')).toBe(originalSha);
  const restoreName = `ez_center_c01_m2_restore_${Date.now()}`;
  expect((await admin.query('SELECT 1 FROM pg_database WHERE datname=$1', [restoreName])).rowCount).toBe(0);
  await admin.query(`CREATE DATABASE "${restoreName}" OWNER center_app`);
  const restoreUrl = new URL(url); restoreUrl.pathname = `/${restoreName}`;
  await run(join(pgBin, 'pg_restore'), ['--no-owner', '--no-privileges', '-d', restoreUrl.href, dump]);
  const restored = createPool({ url: restoreUrl.href });
  try {
    for (const table of ['pgmigrations', 'center_identity', 'users', 'management_audit',
      'providers', 'model_fixed_configs', 'model_settings', 'llm_recording_settings',
      'llm_calls', 'custom_agents', 'plugins', 'plugin_packages']) {
      const row = await restored.query<{ count: string }>(`SELECT count(*) FROM public.${table}`);
      expect(row.rows[0]!.count).toBe(await count(table));
    }
    const restoredAgent = await restored.query<{ prompt: string }>(
      'SELECT prompt FROM public.custom_agents WHERE name=$1', [pure.name]);
    expect(restoredAgent.rows[0]!.prompt).toBe(longPrompt);
  } finally { await restored.end(); }

  const replaced = await upload(await readFile(second));
  expect(replaced.status).toBe(200);
  const invalid = await request(detailUrl, adminToken);
  expect((await invalid.json() as { diagnostics: { status: string }[] }).diagnostics[0]!.status)
    .toBe('capability_missing');
  // 旧引用已失效但原样保留时仍能编辑提示词；新 Agent 引用同一缺失能力则拒绝。
  const preserved = await write(detailUrl, adminToken,
    { ...unknown, description: '更新说明', prompt: '保留旧引用并修改提示词。' },
    'PUT', invalid.headers.get('etag')!);
  expect(preserved.status).toBe(200);
  expect((await write('/agents', adminToken, { ...unknown, name: '新助手' })).status).toBe(409);
  const removed = await request(detailUrl, adminToken, { method: 'DELETE',
    headers: { 'if-match': preserved.headers.get('etag')! } });
  expect(removed.status).toBe(204);
  expect((await request(detailUrl, adminToken)).status).toBe(404);
  expect((await request('/runtime/agents', userToken)).status).toBe(200);
  expect(await count('custom_agents')).toBe('1');
  expect(await count('plugins')).toBe('1');
  expect(await count('plugin_packages')).toBe('1');
  const audits = await pool.query<{ details: object }>('SELECT details FROM public.management_audit');
  expect(JSON.stringify(audits.rows)).not.toContain(longPrompt.slice(0, 100));
  expect(JSON.stringify(audits.rows)).not.toContain('保留旧引用并修改提示词');
}, 30_000);
