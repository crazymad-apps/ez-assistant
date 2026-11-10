import { afterEach, expect, it, vi } from 'vitest';
import type { EntityManager } from 'typeorm';
import { createHttpApp } from '../../dist/app.js';
import { IdentityService } from '../../dist/identity/service.js';
import { organizations } from '../../dist/organizations/repository.js';
import { users } from '../../dist/users/repository.js';

const organizationId = '11111111-1111-4111-8111-111111111111';
const memberId = 2;
const actor = {
  id: 1, username: 'admin', display_name: '管理员', role: 'system_admin' as const, enabled: true, is_super_admin: false,
};
const projection = {
  center_id: 'center', legacy_organization_id: organizationId, expires_at: '2099-01-01T00:00:00Z', user: actor,
};

afterEach(() => vi.restoreAllMocks());

it('系统管理员可在系统范围添加成员，逐项保存角色和状态，并按目标组织撤权与审计', async () => {
  const app = await createHttpApp();
  try {
    const identity = app.get(IdentityService);
    let current: Awaited<ReturnType<typeof organizations.member>>;
    const query = vi.fn(async (sql: string) => {
      if (sql.includes('DELETE FROM public.organization_members')) current = undefined;
    });
    const manager = { query } as unknown as EntityManager;
    vi.spyOn(identity, 'me').mockResolvedValue({ ...projection, scope: { kind: 'system', can_manage: true } });
    vi.spyOn(identity, 'write').mockImplementation(async (work, committed) => {
      const result = await work(manager);
      committed?.(result);
      return result;
    });
    const requireAdmin = vi.spyOn(identity, 'requireAdmin').mockResolvedValue(actor);
    const audit = vi.spyOn(identity, 'audit').mockResolvedValue();
    const revoke = vi.spyOn(identity, 'revokeOrganization').mockImplementation(() => {});
    const organization = { id: organizationId, code: 'alpha', name: '甲组织', enabled: true };
    const getOrganization = vi.spyOn(organizations, 'get').mockResolvedValue(organization);
    vi.spyOn(users, 'byId').mockResolvedValue({
      ...actor, id: memberId, password_hash: '', created_at: new Date(0), updated_at: new Date(0),
    });
    vi.spyOn(organizations, 'member').mockImplementation(async () => current);
    const hasAdmin = vi.spyOn(organizations, 'hasAdmin').mockResolvedValue(false);
    const save = vi.spyOn(organizations, 'saveMember').mockImplementation(async (_, _id, _userId, value) => {
      current = value;
    });

    const request = (method: 'POST' | 'PATCH' | 'DELETE', payload?: object) => app.inject({
      method,
      url: `/api/organizations/${organizationId}/members/${memberId}`,
      headers: { authorization: 'Bearer system-token' },
      payload,
    });
    const patch = (payload: object) => request('PATCH', payload);
    const add = (role: 'member' | 'admin') => request('POST', { role, enabled: true });
    const remove = () => request('DELETE');
    expect((await patch({})).statusCode).toBe(400);
    expect((await patch({ enabled: false })).statusCode).toBe(404);
    expect((await patch({ role: 'admin' })).statusCode).toBe(404);
    getOrganization.mockResolvedValueOnce({ ...organization, enabled: false });
    expect((await add('member')).statusCode).toBe(409);
    expect(save).not.toHaveBeenCalled();

    const added = await add('member');
    expect(added.statusCode).toBe(201);
    expect(added.json()).toEqual({ role: 'member', enabled: true });
    expect((await add('member')).statusCode).toBe(201);
    expect((await add('admin')).json()).toMatchObject({ error: { code: 'ORGANIZATION_MEMBER_EXISTS' } });
    expect((await patch({ role: 'member' })).statusCode).toBe(200);
    expect(audit).toHaveBeenCalledOnce();
    expect((await patch({ role: 'admin' })).json()).toEqual({ role: 'admin', enabled: true });

    const lastAdmin = await patch({ enabled: false });
    expect(lastAdmin.statusCode).toBe(409);
    expect(lastAdmin.json()).toMatchObject({ error: { code: 'ORGANIZATION_ADMIN_REQUIRED' } });
    expect((await remove()).json()).toMatchObject({ error: { code: 'ORGANIZATION_ADMIN_REQUIRED' } });
    expect(query).not.toHaveBeenCalled();
    expect(revoke).not.toHaveBeenCalled();

    hasAdmin.mockResolvedValue(true);
    expect((await patch({ enabled: false })).json()).toEqual({ role: 'admin', enabled: false });
    expect((await patch({ role: 'member' })).json()).toEqual({ role: 'member', enabled: false });
    expect(requireAdmin).toHaveBeenCalledWith(manager, 'system-token');
    expect(save).toHaveBeenCalledTimes(4);
    expect(audit).toHaveBeenCalledTimes(4);
    expect(audit).toHaveBeenCalledWith(
      manager, 'organization_member_saved', expect.any(String), actor.id, undefined, memberId, { enabled: false }, organizationId,
    );
    expect(query).toHaveBeenCalledWith(expect.stringContaining('DELETE FROM public.host_device_users'), [organizationId, memberId]);
    expect(revoke).toHaveBeenCalledExactlyOnceWith(organizationId, memberId);

    expect((await patch({ enabled: true })).statusCode).toBe(200);
    expect((await remove()).statusCode).toBe(204);
    expect((await remove()).statusCode).toBe(204);
    expect((await patch({ role: 'admin' })).statusCode).toBe(404);
    expect(query.mock.calls.filter(([sql]) => sql.includes('DELETE FROM public.organization_members'))).toHaveLength(1);
    expect(query).toHaveBeenCalledWith(expect.stringContaining('DELETE FROM public.organization_members'), [organizationId, memberId]);
    expect(audit).toHaveBeenLastCalledWith(
      manager, 'organization_member_removed', expect.any(String), actor.id, undefined, memberId, {}, organizationId,
    );
    expect(audit).toHaveBeenCalledTimes(6);
    expect(revoke).toHaveBeenCalledTimes(2);
    expect((await add('admin')).statusCode).toBe(201);
  } finally {
    await app.close();
  }
});

it.each(['POST', 'PATCH', 'DELETE'] as const)('组织范围的令牌不能通过系统接口 %s 修改成员关系', async (method) => {
  const app = await createHttpApp();
  try {
    const identity = app.get(IdentityService);
    vi.spyOn(identity, 'me').mockResolvedValue({
      ...projection,
      scope: { kind: 'organization', organization_id: organizationId, code: 'alpha', name: '甲组织', can_manage: true },
    });
    const write = vi.spyOn(identity, 'write');
    const response = await app.inject({
      method,
      url: `/api/organizations/${organizationId}/members/${memberId}`,
      headers: { authorization: 'Bearer organization-token' },
      ...(method === 'DELETE' ? {} : { payload: { role: 'member', enabled: true } }),
    });
    expect(response.statusCode).toBe(403);
    expect(write).not.toHaveBeenCalled();
  } finally {
    await app.close();
  }
});
