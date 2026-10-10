import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { runner } from 'node-pg-migrate';
import { afterAll, expect, it, vi } from 'vitest';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { FeedbackService } from '../../dist/feedback/service.js';
import type { FeedbackDetail, SubmitFeedbackRequest } from '../../dist/contracts/feedback.js';
import type { LoginResponse } from '../../dist/contracts/identity.js';

// 显式场景指向一次性新集群；不接受普通开发连接串，也不清理或重用既有数据库。
const root = resolve(process.env.CENTER_F04_SCENARIO ?? '');
if (!process.env.CENTER_F04_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f04-m0-'))) throw new Error('须指定新建 F04 M0 场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8')) as {
  root: string; postgres_host: string; postgres_port: number; database: string; role: string; center_port: number;
};
if (scenario.root !== root || scenario.postgres_host !== '127.0.0.1' || scenario.database !== 'ez_center_f04_m0'
  || scenario.role !== 'center_app') throw new Error('隔离目标不符');
const pool = createPool({ url: `postgres://center_app@127.0.0.1:${scenario.postgres_port}/${scenario.database}` });
const source = createDataSource({ url: `postgres://center_app@127.0.0.1:${scenario.postgres_port}/${scenario.database}` });
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
const origin = `http://127.0.0.1:${scenario.center_port}`;
const headers = (token: string) => ({ authorization: `Bearer ${token}` });
const body = (extra: Partial<SubmitFeedbackRequest> = {}): SubmitFeedbackRequest => ({
  submission_id: randomUUID(), source: 'manual', title: '平台 %_ 中文反馈', description: '第一行\n第二行 <img src="https://example.invalid/a">',
  reproduction: null, expected_result: null,
  target: { kind: 'platform', reference: 'ez-assistant', name: '用户提供名称', version: null, content_ref: null, location: '设置页面' },
  environment: { host_version: '0.29.0', host_os: 'macos', host_arch: 'aarch64', client_kind: 'web', client_version: null, client_os: null, client_arch: null },
  ...extra,
});
const auditCount = async (action: string) => Number((await pool.query('SELECT COUNT(*) FROM public.management_audit WHERE action=$1 AND success', [action])).rows[0].count);
const feedbackCount = async () => Number((await pool.query('SELECT COUNT(*) FROM public.feedback')).rows[0].count);
async function login(username = 'admin', password = '123456') {
  const response = await app!.inject({ method: 'POST', url: '/api/auth/login', payload: { username, password } });
  expect(response.statusCode).toBe(200);
  return response.json<LoginResponse>().token;
}
afterAll(async () => {
  vi.restoreAllMocks();
  await app?.close();
  if (source.isInitialized) await source.destroy();
  await pool.end();
});

it('真实 PostgreSQL 升级、身份隔离、幂等、原值处理冲突、审计原子性与重启持久化', async () => {
  const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0];
  expect(actual).toEqual({ host: scenario.postgres_host, port: scenario.postgres_port, db: scenario.database, role: scenario.role, dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT COUNT(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  const migrations = upgradeFiles();
  const connection = await pool.connect();
  try {
    await runner({ dbClient: connection, dir: migrations.directory, direction: 'up', count: 5,
      migrationsTable: 'pgmigrations', schema: 'public', migrationsSchema: 'public', singleTransaction: true,
      logger: { info() {}, warn() {}, error() {} } });
  } finally { connection.release(true); }
  await pool.query("UPDATE public.users SET display_name='F04 旧数据夹具' WHERE username='admin'");
  const before = (await pool.query('SELECT * FROM public.users')).rows;
  const originalIdentity = (await pool.query('SELECT * FROM public.center_identity')).rows;
  const originalAudit = (await pool.query('SELECT * FROM public.management_audit')).rows;
  expect((await upgradeDatabase(pool)).applied).toEqual(['006-feedback']);
  expect((await pool.query('SELECT * FROM public.users')).rows).toEqual(before);
  expect((await pool.query('SELECT * FROM public.center_identity')).rows).toEqual(originalIdentity);
  expect((await pool.query('SELECT * FROM public.management_audit')).rows).toEqual(originalAudit);
  expect(await feedbackCount()).toBe(0);
  expect((await upgradeDatabase(pool)).applied).toEqual([]);
  await source.initialize();
  app = await createHttpApp({ source, origin });
  let admin = await login();
  expect((await app.inject('/api/info')).json().capabilities).toContain('feedback');
  for (const username of ['feedback-a', 'feedback-b']) {
    expect((await app.inject({ method: 'POST', url: '/api/users', headers: headers(admin), payload: {
      username, display_name: username, role: 'user', enabled: true, password: 'test1234',
    } })).statusCode).toBe(201);
  }
  let a = await login('feedback-a', 'test1234');
  const b = await login('feedback-b', 'test1234');
  const submit = (payload: object, token = a) => app!.inject({ method: 'POST', url: '/api/feedback', headers: headers(token), payload });
  const input = body();
  const repeated = await Promise.all(Array.from({ length: 8 }, () => submit(input)));
  expect(repeated.map(item => item.statusCode)).toEqual(Array(8).fill(200));
  const record = repeated[0]!.json<FeedbackDetail>();
  expect(new Set(repeated.map(item => item.json<FeedbackDetail>().id)).size).toBe(1);
  expect(record.target.name).toBe('EZ Assistant');
  expect(record.owner_user_id).not.toBe(1);
  expect(await feedbackCount()).toBe(1);
  expect(await auditCount('feedback_created')).toBe(1);
  expect((await submit({ ...input, title: ` ${input.title} `, environment: { ...input.environment, host_version: 'future' } })).json<FeedbackDetail>().environment.host_version).toBe('0.29.0');
  expect((await submit({ ...input, title: '不同内容' })).statusCode).toBe(409);
  expect((await submit({ ...input, owner_user_id: 1 })).statusCode).toBe(400);
  expect((await submit({ ...input, environment: { ...input.environment, secret: '禁止任意字段' } })).statusCode).toBe(400);
  expect((await submit({ ...input, description: '中'.repeat(4000), reproduction: '中'.repeat(1000) })).statusCode).toBe(413);
  expect((await submit({ ...input, description: '😀'.repeat(4001) })).statusCode).toBeGreaterThanOrEqual(400);
  expect((await app.inject({ url: `/api/feedback/${record.id}`, headers: headers(b) })).statusCode).toBe(404);
  expect((await app.inject({ url: `/api/feedback/${randomUUID()}`, headers: headers(b) })).json().error.code).toBe('FEEDBACK_NOT_FOUND');
  expect((await app.inject({ url: '/api/admin/feedback', headers: headers(a) })).statusCode).toBe(403);
  expect((await app.inject({ url: '/api/feedback', headers: headers(b) })).json().total).toBe(0);
  expect((await submit(input, b)).statusCode).toBe(200);
  const plugin = await submit(body({ target: { kind: 'plugin', reference: 'unpublished-plugin', name: '同名对象', version: '1.2.3', content_ref: null, location: 'tool.paint' }, source: 'agent' }));
  expect(plugin.statusCode).toBe(200);
  for (const reference of [null, 'scope-a:kb1', 'scope-b:kb1']) {
    expect((await submit(body({ target: { kind: 'knowledge', reference, name: '同名对象', version: null, content_ref: 'doc:42', location: '第三章' } }))).statusCode).toBe(200);
  }
  const list = (query: string, token = admin, prefix = '/api/admin/feedback') => app!.inject({ url: `${prefix}?${query}`, headers: headers(token) });
  expect((await list('target_kind=knowledge&target_unresolved=true')).json().total).toBe(1);
  expect((await list('target_kind=knowledge&target_reference=scope-a%3Akb1')).json().total).toBe(1);
  expect((await list('target_kind=plugin&target_reference=unpublished-plugin&source=agent')).json().total).toBe(1);
  expect((await list('title=%25_')).json().total).toBe(6);
  expect((await list('title=%25_nonexistent')).json().total).toBe(0);
  expect((await list('offset=20')).json()).toMatchObject({ items: [], total: 6, limit: 20, offset: 20 });
  for (const query of ['limit=21', 'status=pending&status=closed', 'owner_user_id=0', 'target_reference=abc', 'target_kind=plugin&target_unresolved=true', 'unknown=1'])
    expect((await list(query)).statusCode).toBe(400);
  const own = await list(`submission_id=${input.submission_id}`, a, '/api/feedback');
  expect(own.json().total).toBe(1);
  expect(own.json().items[0]).not.toHaveProperty('description');
  expect(own.json().items[0]).not.toHaveProperty('environment');
  expect(Buffer.byteLength((await list('')).body)).toBeLessThan(65536);
  const handle = (payload: object) => app!.inject({ method: 'PATCH', url: `/api/admin/feedback/${record.id}`, headers: headers(admin), payload });
  const patch = { expected_status: 'pending', expected_handling_note: null, status: 'processing', handling_note: '正在排查' };
  const race = await Promise.all([handle(patch), handle({ ...patch, handling_note: '另一个管理员的处理' })]);
  expect(race.map(item => item.statusCode).sort()).toEqual([200, 409]);
  const winner = race.find(item => item.statusCode === 200)!.json<FeedbackDetail>();
  expect(await auditCount('feedback_handled')).toBe(1);
  expect((await handle({ ...patch, handling_note: winner.handling_note })).statusCode).toBe(200);
  expect(await auditCount('feedback_handled')).toBe(1);
  expect((await handle({ ...patch, status: 'resolved', handling_note: ' ' })).statusCode).toBe(400);
  expect((await submit(input)).json<FeedbackDetail>().status).toBe('processing');
  const identity = app.get(IdentityService);
  const audit = vi.spyOn(identity, 'audit').mockRejectedValueOnce(new Error('模拟审计故障'));
  expect((await submit(body())).statusCode).toBe(503);
  expect(await feedbackCount()).toBe(6);
  expect(await auditCount('feedback_created')).toBe(6);
  audit.mockRejectedValueOnce(new Error('模拟处理审计故障'));
  expect((await handle({ expected_status: winner.status, expected_handling_note: winner.handling_note, status: 'resolved', handling_note: '故障不应保存' })).statusCode).toBe(503);
  expect((await app.inject({ url: `/api/feedback/${record.id}`, headers: headers(a) })).json().handling_note).toBe(winner.handling_note);
  audit.mockRestore();
  expect((await handle({ expected_status: winner.status, expected_handling_note: winner.handling_note, status: 'resolved', handling_note: '已修复\n请升级后重试' })).statusCode).toBe(200);
  const completed = (await app.inject({ url: `/api/feedback/${record.id}`, headers: headers(a) })).json<FeedbackDetail>();
  expect(completed).toMatchObject({ status: 'resolved', handler_user_id: 1, handling_note: '已修复\n请升级后重试' });
  const auditRows = (await pool.query("SELECT details FROM public.management_audit WHERE action LIKE 'feedback_%'")).rows;
  expect(JSON.stringify(auditRows)).not.toContain('排查');
  expect(JSON.stringify(auditRows)).not.toContain('第一行');
  // 注销排在已通过外层认证的创建之前，事务内复核必须拒绝副作用。
  let release!: () => void;
  const blocker = identity.write(async () => new Promise<void>(done => { release = done; }));
  await vi.waitFor(() => expect(release).toBeTypeOf('function'));
  const logout = identity.logout(a, randomUUID());
  const queued = app.get(FeedbackService).submit(a, body(), randomUUID());
  const rejected = expect(queued).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  release(); await blocker; await logout; await rejected;
  expect(await feedbackCount()).toBe(6);
  a = await login('feedback-a', 'test1234');
  expect((await app.inject({ method: 'PATCH', url: `/api/users/${record.owner_user_id}`, headers: headers(admin), payload: { enabled: false } })).statusCode).toBe(200);
  expect((await submit(body())).statusCode).toBe(401);
  expect((await app.inject({ url: '/api/feedback', headers: headers(a) })).statusCode).toBe(401);
  expect((await app.inject({ method: 'PATCH', url: `/api/users/${record.owner_user_id}`, headers: headers(admin), payload: { enabled: true } })).statusCode).toBe(200);
  await app.close();
  app = await createHttpApp({ source, origin });
  admin = await login(); a = await login('feedback-a', 'test1234');
  const restarted = await submit(input);
  expect(restarted.statusCode).toBe(200);
  expect(restarted.json()).toMatchObject({ id: record.id, status: 'resolved' });
  expect(await feedbackCount()).toBe(6);
  expect(await auditCount('feedback_created')).toBe(6);
  expect(await auditCount('feedback_handled')).toBe(2);
  expect((await pool.query('SELECT COUNT(*) FROM public.users')).rows[0].count).toBe('3');
  expect((await pool.query('SELECT COUNT(*) FROM public.pgmigrations')).rows[0].count).toBe('6');
}, 60000);

it('反馈详情读取途中撤销身份时不发布已读取的数据', async () => {
  const actual = (await pool.query("SELECT current_database() AS db,inet_server_port() AS port,current_setting('data_directory') AS dir")).rows[0];
  expect(actual).toEqual({ db: scenario.database, port: scenario.postgres_port, dir: join(root, 'pgdata') });
  if (!source.isInitialized) await source.initialize();
  app ??= await createHttpApp({ source, origin });
  const token = await login('feedback-b', 'test1234');
  const record = (await app.inject({ url: '/api/feedback', headers: headers(token) })).json<{ items: FeedbackDetail[] }>().items[0]!;
  const identity = app.get(IdentityService);
  const original = identity.requireUser.bind(identity);
  const check = vi.spyOn(identity, 'requireUser').mockImplementationOnce(async (manager, credential) => {
    const owner = await original(manager, credential);
    identity.revokeUser(owner.id);
    return owner;
  });
  try {
    await expect(app.get(FeedbackService).get(token, record.id, false)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
    expect(check).toHaveBeenCalledTimes(2);
  } finally { check.mockRestore(); }
  expect(await feedbackCount()).toBe(6);
});
