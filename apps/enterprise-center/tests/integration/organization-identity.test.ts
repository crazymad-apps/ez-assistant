import { join } from 'node:path';
import { realpathSync } from 'node:fs';
import { runner } from 'node-pg-migrate';
import { decodeJwt } from 'jose';
import { expect, it } from 'vitest';
import { createHttpApp } from '../../dist/app.js';
import { createPool } from '../../dist/database/pool.js';
import { createDataSource } from '../../dist/database/source.js';
import { upgradeDatabase } from '../../dist/database/upgrade.js';
import { upgradeFiles } from '../../dist/database/migrations.js';
import { IdentityService } from '../../dist/identity/service.js';
import type { LoginResponse, ScopeView } from '../../dist/contracts/identity.js';
import type { MemberPage, Organization } from '../../dist/contracts/organizations.js';
import type { UserPage, UserRecord } from '../../dist/contracts/users.js';

const url = process.env.CENTER_F01_M0_DATABASE_URL;
const home = process.env.CENTER_F01_M0_TEST_HOME;
const local = join(process.cwd(), '../../.runtime-test');
if (!url || !home || !realpathSync(home).startsWith(`${realpathSync(local)}/v033-f01-m0.`))
  throw new Error('需要新建且显式指定的 F01 M0 隔离库。');
const target = new URL(url);
if (target.hostname !== '127.0.0.1' || !target.port || target.pathname !== '/ez_center_v033_f01_m0' ||
    target.username !== 'f01_m0' || target.search || target.hash)
  throw new Error('隔离库目标不匹配。');

it('012 升级后通过正式 API 验证范围、成员及旧身份保护', async () => {
  const pool = createPool({ url });
  const source = createDataSource({ url });
  let app: Awaited<ReturnType<typeof createHttpApp>> | undefined;
  try {
    const verified = await pool.query(`SELECT host(inet_server_addr()) AS host, inet_server_port() AS port,
      current_database() AS db, current_user AS role, current_setting('data_directory') AS dir,
      (SELECT count(*) FROM pg_tables WHERE schemaname = 'public') AS tables`);
    expect(verified.rows[0]).toEqual({ host: target.hostname, port: Number(target.port), db: target.pathname.slice(1),
      role: target.username, dir: join(home, 'pgdata'), tables: '0' });
    const old = await pool.connect();
    try {
      const files = upgradeFiles();
      expect(files.names[12]).toBe('013-organizations');
      const applied = await runner({ dbClient: old, dir: files.directory, direction: 'up', count: 12,
        migrationsTable: 'pgmigrations', schema: 'public', migrationsSchema: 'public', singleTransaction: true,
        logger: { info() {}, warn() {}, error() {} } });
      expect(applied).toHaveLength(12);
      await old.query(`INSERT INTO public.users(username, display_name, role, enabled, password_hash)
        SELECT fixture.username, fixture.username, fixture.role, fixture.enabled, admin.password_hash
        FROM public.users admin CROSS JOIN (VALUES ('alice', 'user', true), ('bob', 'user', true),
          ('old-admin', 'admin', false), ('old-member', 'user', false)) fixture(username, role, enabled)
        WHERE admin.username = 'admin'`);
    } finally {
      old.release(true);
    }
    const legacy = await pool.query('SELECT id, username, enabled, is_super_admin, role FROM public.users ORDER BY id');
    const centerBefore = await pool.query('SELECT center_id FROM public.center_identity');
    expect(legacy.rowCount).toBe(5);
    expect((await upgradeDatabase(pool)).applied).toEqual(['013-organizations', '014-organization-resources', '015-organization-model-policy']);
    expect((await pool.query('SELECT count(*) FROM public.organizations')).rows[0].count).toBe('1');
    expect((await pool.query('SELECT count(*) FROM public.pgmigrations')).rows[0].count).toBe('15');
    expect((await pool.query('SELECT count(*) FROM public.management_audit')).rows[0].count).toBe('1');
    const center = (await pool.query('SELECT center_id, legacy_organization_id FROM public.center_identity')).rows[0];
    expect(center.center_id).toBe(centerBefore.rows[0].center_id);
    expect((await pool.query('SELECT id, username, enabled, is_super_admin, role FROM public.users ORDER BY id')).rows)
      .toEqual(legacy.rows.map(row => ({ ...row, role: row.role === 'admin' ? 'system_admin' : row.role })));
    expect((await pool.query('SELECT organization_id, user_id, role, enabled FROM public.organization_members ORDER BY user_id')).rows)
      .toEqual(legacy.rows.map(row => ({ organization_id: center.legacy_organization_id, user_id: row.id, role: 'member', enabled: true })));

    await source.initialize();
    app = await createHttpApp({ source, origin: 'http://127.0.0.1:7320' });
    const running = app;
    const request = (method: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE', path: string, token?: string, payload?: object) =>
      running.inject({ method, url: `/api/${path}`, headers: token ? { authorization: `Bearer ${token}` } : {}, payload });
    const signIn = async (username: string) => {
      const response = await request('POST', 'auth/login', undefined, { username, password: '123456' });
      expect(response.statusCode).toBe(200);
      return response.json<LoginResponse>();
    };
    const select = (login: LoginResponse, kind: 'system' | 'organization', organization_id?: string, entry = 'admin') =>
      request('POST', 'auth/scope', login.token, { kind, ...(organization_id ? { organization_id } : {}), entry });
    const first = await signIn('admin');
    expect(first).toMatchObject({ scope: { kind: 'unselected' }, legacy_organization_id: center.legacy_organization_id,
      user: { role: 'system_admin', is_super_admin: true } });
    expect(first.llm_key).toBeUndefined();
    const claims = decodeJwt(first.token);
    expect(claims.exp! - claims.iat!).toBe(604800);
    expect(new Date(first.expires_at).getTime()).toBe(claims.exp! * 1000);
    expect((await request('GET', 'users', first.token)).statusCode).toBe(403);
    expect((await request('GET', 'auth/me', 'ct_' + 'a'.repeat(64))).statusCode).toBe(401);
    const parts = first.token.split('.');
    parts[1] = Buffer.from(JSON.stringify({ ...claims, scope: { kind: 'system' } })).toString('base64url');
    expect((await request('GET', 'auth/me', parts.join('.'))).statusCode).toBe(401);
    const concurrent = await Promise.all([select(first, 'system'), select(first, 'system')]);
    expect(concurrent.map(response => response.statusCode).sort()).toEqual([200, 401]);
    const system = concurrent.find(response => response.statusCode === 200)!.json<LoginResponse>();
    const same = (await select(system, 'system')).json<LoginResponse>();
    expect(same).toEqual(system);
    expect((await request('GET', 'auth/me', first.token)).statusCode).toBe(401);

    const listed = await request('GET', 'users', system.token);
    expect(listed.statusCode).toBe(200);
    const accounts = listed.json<UserPage>().items;
    expect(accounts).toHaveLength(5);
    const alice = accounts.find(row => row.username === 'alice')!;
    const bob = accounts.find(row => row.username === 'bob')!;
    expect(alice.organizations).toMatchObject([{ id: center.legacy_organization_id, role: 'member' }]);
    const created = await request('POST', 'organizations', system.token,
      { code: 'alpha', name: '同名组织', administrator_user_id: alice.id });
    expect(created.statusCode).toBe(201);
    const alpha = created.json<Organization>();
    const createdBeta = await request('POST', 'organizations', system.token,
      { code: 'beta', name: '同名组织', administrator_user_id: bob.id });
    expect(createdBeta.statusCode).toBe(201);
    const beta = createdBeta.json<Organization>();
    expect((await request('PATCH', `users/${alice.id}`, system.token, { enabled: false })).statusCode).toBe(409);
    expect((await request('DELETE', `users/${bob.id}`, system.token)).statusCode).toBe(409);
    expect((await request('PATCH', `users/${first.user.id}`, system.token, { role: 'user' })).statusCode).toBe(409);

    const ordinary = await request('POST', 'users', system.token, {
      username: 'no-org', display_name: '无组织账号', role: 'user', enabled: true, password: 'test-pass1',
    });
    expect(ordinary.statusCode).toBe(201);
    expect(ordinary.json<UserRecord>().role).toBe('user');
    const ordinaryLogin = await request('POST', 'auth/login', undefined, { username: 'no-org', password: 'test-pass1' });
    expect((await request('GET', 'auth/scopes?entry=admin', ordinaryLogin.json<LoginResponse>().token)).json()).toEqual([]);
    const aliceLogin = await signIn('alice');
    expect((await request('GET', 'auth/scopes?entry=admin', aliceLogin.token)).json<ScopeView[]>()
      .map(scope => scope.organization_id)).toEqual([alpha.id]);
    expect((await request('GET', 'auth/scopes?entry=client', aliceLogin.token)).json<ScopeView[]>()
      .map(scope => scope.organization_id)).toEqual([alpha.id, center.legacy_organization_id]);
    expect((await select(aliceLogin, 'organization', beta.id)).statusCode).toBe(403);
    expect((await select(system, 'organization', alpha.id, 'client')).statusCode).toBe(403);
    const alphaManager = (await select(system, 'organization', alpha.id)).json<LoginResponse>();
    expect(alphaManager.scope).toMatchObject({ kind: 'organization', organization_id: alpha.id, can_manage: true });
    expect(alphaManager.scope.member_role).toBeUndefined();
    expect(alphaManager.llm_key).toBeUndefined();
    expect((await request('GET', 'users', alphaManager.token)).statusCode).toBe(403);
    expect((await request('GET', 'model-providers', alphaManager.token)).statusCode).toBe(200);
    expect((await request('POST', 'llm/providers/00000000-0000-0000-0000-000000000001/v1/responses', alphaManager.token, {})).statusCode)
      .toBe(401);
    const alphaAdmin = (await select(aliceLogin, 'organization', alpha.id)).json<LoginResponse>();
    const members = await request('GET', 'organization-members', alphaAdmin.token);
    expect(members.statusCode).toBe(200);
    expect(members.json<MemberPage>().items).toEqual([{
      user_id: alice.id, username: 'alice', display_name: 'alice', role: 'admin', enabled: true, account_enabled: true,
    }]);
    expect((await request('GET', `organization-members?organization_id=${beta.id}`, alphaAdmin.token)).statusCode).toBe(400);
    expect((await request('PUT', `organization-members/${bob.id}`, alphaAdmin.token, { role: 'admin', enabled: true })).statusCode).toBe(403);
    expect((await request('DELETE', `organization-members/${alice.id}`, alphaManager.token)).statusCode).toBe(409);
    expect((await request('PUT', `organization-members/${bob.id}`, alphaManager.token, { role: 'admin', enabled: true })).statusCode).toBe(204);

    const otherLogin = await signIn('alice');
    const client = (await select(otherLogin, 'organization', center.legacy_organization_id, 'client')).json<LoginResponse>();
    const identity = running.get(IdentityService);
    expect(client.llm_key).toMatch(/^cl_/);
    await expect(identity.llmIdentity(client.llm_key!)).resolves.toMatchObject({ scope: { organization_id: center.legacy_organization_id } });
    expect((await request('GET', 'organization-members', client.token)).statusCode).toBe(403);
    expect((await request('PUT', `organization-members/${alice.id}`, alphaManager.token, { role: 'member', enabled: true })).statusCode).toBe(204);
    expect((await request('GET', 'auth/me', alphaAdmin.token)).json()).toMatchObject({
      scope: { member_role: 'member', can_manage: false },
    });
    expect((await request('GET', 'organization-members', alphaAdmin.token)).statusCode).toBe(403);
    expect((await request('PUT', `organization-members/${alice.id}`, alphaManager.token, { role: 'member', enabled: false })).statusCode).toBe(204);
    expect((await request('GET', 'auth/me', alphaAdmin.token)).statusCode).toBe(401);
    expect((await request('GET', 'auth/me', client.token)).statusCode).toBe(200);
    const systemAgain = (await select(alphaManager, 'system')).json<LoginResponse>();
    const disabled = await request('PATCH', `organizations/${alpha.id}`, systemAgain.token,
      { name: alpha.name, code: alpha.code, enabled: false });
    expect(disabled.statusCode).toBe(200);
    expect(disabled.json<Organization>().enabled).toBe(false);
    const restarted = await createHttpApp({ source, origin: 'http://127.0.0.1:7320' });
    try {
      expect((await restarted.inject({ method: 'GET', url: '/api/auth/me', headers: { authorization: `Bearer ${systemAgain.token}` } })).statusCode)
        .toBe(401);
    } finally {
      await restarted.close();
    }
    expect((await request('POST', 'auth/logout', client.token, {})).statusCode).toBe(204);
    await expect(identity.llmIdentity(client.llm_key!)).rejects.toMatchObject({ code: 'TOKEN_INVALID' });
    expect((await pool.query('SELECT count(*) FROM public.center_identity')).rows[0].count).toBe('1');
    expect((await pool.query('SELECT count(*) FROM public.users')).rows[0].count).toBe('6');
    expect((await pool.query('SELECT count(*) FROM public.organizations')).rows[0].count).toBe('3');
    expect((await pool.query('SELECT count(*) FROM public.organization_members')).rows[0].count).toBe('8');
  } finally {
    await app?.close();
    if (source.isInitialized) await source.destroy();
    await pool.end();
  }
}, 30000);
