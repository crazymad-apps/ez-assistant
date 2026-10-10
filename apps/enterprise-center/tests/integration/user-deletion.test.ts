import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { afterAll, expect, it, vi } from 'vitest';
import { runner } from 'node-pg-migrate';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import type { LoginResponse } from '../../dist/contracts/identity.js';
import type { UserRecord } from '../../dist/contracts/users.js';

const root = resolve(process.env.CENTER_F05_SCENARIO ?? '');
if (!process.env.CENTER_F05_SCENARIO || !root.startsWith(resolve('../../.runtime-test/f05-delete-')))
  throw new Error('须指定新建 F05 隔离场景');
const scenario = JSON.parse(await readFile(join(root, 'scenario.json'), 'utf8'));
if (scenario.root !== root || scenario.database !== 'ez_center_f05' || scenario.role !== 'center_app')
  throw new Error('隔离目标不符');
const config = { url: `postgres://center_app@127.0.0.1:${scenario.port}/ez_center_f05` };
const pool = createPool(config), source = createDataSource(config);
let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
afterAll(async () => { vi.restoreAllMocks(); await app?.close(); if (source.isInitialized) await source.destroy(); await pool.end(); });
const password = 'safe-password-123';
async function login(username = 'admin', secret = '123456') {
  const r = await app!.inject({ method: 'POST', url: '/api/auth/login', payload: { username, password: secret } });
  expect(r.statusCode).toBe(200); return r.json<LoginResponse>();
}
function request(token: string, method: 'GET' | 'POST' | 'DELETE', url: string, payload?: object) {
  return app!.inject({ method, url, headers: { authorization: `Bearer ${token}` }, ...(payload ? { payload } : {}) });
}
async function create(token: string, username: string, role: 'admin' | 'user' = 'user') {
  const r = await request(token, 'POST', '/api/users', { username, display_name: username, password, role, enabled: true });
  expect(r.statusCode).toBe(201); return r.json<UserRecord>();
}

it('旧结构迁移保留审计；删除回滚、凭据失效、历史归属与同名身份隔离', async () => {
  const actual = (await pool.query("SELECT host(inet_server_addr()) AS host,inet_server_port() AS port,current_database() AS db,current_user AS role,current_setting('data_directory') AS dir")).rows[0];
  expect(actual).toEqual({ host: '127.0.0.1', port: scenario.port, db: 'ez_center_f05', role: 'center_app', dir: join(root, 'pgdata') });
  expect((await pool.query("SELECT count(*) FROM pg_tables WHERE schemaname='public'")).rows[0].count).toBe('0');
  const client = await pool.connect();
  try {
    await runner({ dbClient: client, dir: upgradeFiles().directory, direction: 'up', count: 8,
      migrationsTable: 'pgmigrations', schema: 'public', migrationsSchema: 'public', singleTransaction: true,
      logger: { info() {}, warn() {}, error() {} } });
  } finally { client.release(true); }
  const oldAudit = (await pool.query('SELECT * FROM management_audit ORDER BY id')).rows;
  expect((await upgradeDatabase(pool)).applied).toEqual(['009-user-deletion']);
  expect((await pool.query('SELECT * FROM management_audit ORDER BY id')).rows).toEqual(oldAudit);
  await source.initialize(); app = await createHttpApp({ source, origin: 'http://127.0.0.1:7339' });
  const identity = app.get(IdentityService), admin = await login();
  const user = await create(admin.token, 'deleted-user'), owner = await login(user.username, password), second = await login(user.username, password);
  const revoked: (number | null)[] = []; const unsubscribe = identity.onRevoked(id => revoked.push(id));
  const submission = await request(owner.token, 'POST', '/api/feedback', {
    submission_id: randomUUID(), source: 'manual', title: '保留反馈', description: '账号删除后仍保留', reproduction: null, expected_result: null,
    target: { kind: 'platform', reference: 'ez-assistant', name: '平台', version: null, content_ref: null, location: null },
    environment: { host_version: '0.29.0', host_os: 'macos', host_arch: 'aarch64', client_kind: 'web', client_version: null, client_os: null, client_arch: null },
  });
  expect(submission.statusCode).toBe(200); const feedback = submission.json();
  const call = randomUUID();
  await pool.query(`INSERT INTO llm_calls(id,user_id,username,kind,protocol,outcome,request_snapshot_state,response_snapshot_state)
    VALUES ($1,$2,$3,'proxy','open_ai_responses','forwarded','disabled','disabled')`, [call, user.id, user.username]);
  const auditsBefore = Number((await pool.query('SELECT count(*) FROM management_audit')).rows[0].count);
  const spy = vi.spyOn(identity, 'audit').mockRejectedValueOnce(new Error('injected audit failure'));
  expect((await request(admin.token, 'DELETE', `/api/users/${user.id}`)).statusCode).toBe(503);
  spy.mockRestore();
  expect((await pool.query('SELECT count(*) FROM users WHERE id=$1',[user.id])).rows[0].count).toBe('1');
  expect(Number((await pool.query('SELECT count(*) FROM management_audit')).rows[0].count)).toBe(auditsBefore);
  expect((await request(owner.token, 'GET', '/api/auth/me')).statusCode).toBe(200);
  expect(revoked).not.toContain(user.id);
  expect((await request(owner.token, 'DELETE', `/api/users/${admin.user.id}`)).statusCode).toBe(403);
  expect((await request(admin.token, 'DELETE', `/api/users/${admin.user.id}`)).statusCode).toBe(409);
  expect((await request(admin.token, 'DELETE', '/api/users/invalid')).statusCode).toBe(400);
  expect((await request(admin.token, 'DELETE', `/api/users/${user.id}`)).statusCode).toBe(204);
  expect(revoked).toContain(user.id); unsubscribe();
  for (const credentials of [owner, second]) {
    expect((await request(credentials.token, 'GET', '/api/auth/me')).statusCode).toBe(401);
    await expect(identity.llmIdentity(credentials.llm_key)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
  }
  expect((await request(admin.token, 'DELETE', `/api/users/${user.id}`)).statusCode).toBe(404);
  expect((await app.inject({method:'POST',url:'/api/auth/login',payload:{username:user.username,password}})).statusCode).toBe(401);
  expect((await pool.query('SELECT user_id,username FROM llm_calls WHERE id=$1',[call])).rows[0]).toEqual({user_id:user.id,username:user.username});
  expect((await request(admin.token,'GET',`/api/admin/feedback/${feedback.id}`)).json().owner_user_id).toBe(user.id);
  const replacement = await create(admin.token,user.username);expect(replacement.id).not.toBe(user.id);
  const fresh = await login(replacement.username,password);
  expect((await request(fresh.token,'GET',`/api/feedback/${feedback.id}`)).statusCode).toBe(404);
  expect((await request(fresh.token,'GET','/api/feedback')).json().total).toBe(0);
  const oldRefs = (await pool.query('SELECT target_user_id FROM management_audit WHERE action=$1 AND success',['user_deleted'])).rows;
  expect(oldRefs).toEqual([{target_user_id:user.id}]);
  expect((await pool.query('SELECT count(*) FROM users WHERE id=$1',[user.id])).rows[0].count).toBe('0');
});

it('最后管理员保护、重复并发删除及本人删除撤销身份', async () => {
  const admin = await login(), other = await create(admin.token,'ordinary-admin','admin');
  const credentials = await login(other.username,password);
  await pool.query('UPDATE users SET enabled=false WHERE id=$1',[admin.user.id]);
  const last = await request(credentials.token,'DELETE',`/api/users/${other.id}`);
  expect(last.statusCode).toBe(409);expect(last.json().error.code).toBe('LAST_ADMIN_REQUIRED');
  await pool.query('UPDATE users SET enabled=true WHERE id=$1',[admin.user.id]);
  const victim = await create(admin.token,'concurrent-delete');
  const results = await Promise.all([request(admin.token,'DELETE',`/api/users/${victim.id}`),request(admin.token,'DELETE',`/api/users/${victim.id}`)]);
  expect(results.map(r=>r.statusCode).sort()).toEqual([204,404]);
  expect((await pool.query("SELECT count(*) FROM management_audit WHERE action='user_deleted' AND success AND target_user_id=$1",[victim.id])).rows[0].count).toBe('1');
  expect((await request(credentials.token,'DELETE',`/api/users/${other.id}`)).statusCode).toBe(204);
  expect((await request(credentials.token,'GET','/api/auth/me')).statusCode).toBe(401);
  const audits = (await request(admin.token,'GET','/api/audit?action=user_deleted')).json();
  expect(audits.items.some((x:{actor_user_id:number;target_user_id:number})=>x.actor_user_id===other.id&&x.target_user_id===other.id)).toBe(true);
});
