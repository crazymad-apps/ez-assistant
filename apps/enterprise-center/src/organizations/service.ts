import { QueryFailedError } from 'typeorm';
import type { DataSource, EntityManager } from 'typeorm';
import type { CreateOrganization, MemberCandidate, MemberQuery, Organization, SaveMember, UpdateMember, UpdateOrganization } from '../contracts/organizations.js';
import { IdentityError } from '../identity/errors.js';
import type { IdentityService } from '../identity/service.js';
import { users } from '../users/repository.js';
import { organizations } from './repository.js';

type MemberChange =
  | { kind: 'organization'; input: SaveMember | undefined }
  | { kind: 'add'; organizationId: string; input: SaveMember }
  | { kind: 'update'; organizationId: string; patch: UpdateMember }
  | { kind: 'remove'; organizationId: string };

export class OrganizationsService {
  constructor(private readonly identity: IdentityService, private readonly connection?: DataSource) {}

  private get manager(): EntityManager {
    if (!this.connection) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.connection.manager;
  }

  async list(token?: string): Promise<Organization[]> {
    const rows = await organizations.list(this.manager);
    await this.identity.requireAdmin(this.manager, token);
    return rows;
  }

  private async writeOrganization(query: () => Promise<Organization[]>): Promise<Organization> {
    try {
      const [row] = await query();
      return row!;
    } catch (error) {
      if (error instanceof QueryFailedError && Reflect.get(error.driverError, 'constraint') === 'organizations_code_key')
        throw new IdentityError(409, 'ORGANIZATION_CODE_EXISTS');
      throw error;
    }
  }

  create(token: string | undefined, input: CreateOrganization, requestId: string) {
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const admin = await users.byId(manager, input.administrator_user_id);
      if (!admin?.enabled) throw new IdentityError(409, 'ORGANIZATION_ADMIN_REQUIRED');
      const org = await this.writeOrganization(() => manager.query(
        'INSERT INTO public.organizations(code, name) VALUES ($1, $2) RETURNING id, code, name, enabled',
        [input.code, input.name],
      ));
      await manager.query('INSERT INTO public.llm_recording_settings(organization_id) VALUES ($1)', [org.id]);
      await organizations.saveMember(manager, org.id, admin.id, { role: 'admin', enabled: true });
      await this.identity.audit(manager, 'organization_created', requestId, actor.id, undefined, admin.id,
        { name: org.name, enabled: true });
      await this.identity.audit(manager, 'organization_member_saved', requestId, actor.id, undefined, admin.id,
        { enabled: true }, org.id);
      return org;
    });
  }

  update(token: string | undefined, id: string, input: UpdateOrganization, requestId: string) {
    return this.identity.write(async manager => {
      const actor = await this.identity.requireAdmin(manager, token);
      const before = await organizations.get(manager, id);
      if (!before) throw new IdentityError(404, 'ORGANIZATION_NOT_FOUND');
      if (!before.enabled && input.enabled && !await organizations.hasAdmin(manager, id))
        throw new IdentityError(409, 'ORGANIZATION_ADMIN_REQUIRED');
      if (before.enabled === input.enabled && before.name === input.name && before.code === input.code)
        return { org: before, revoked: false };
      const org = await this.writeOrganization(async () => {
        const [rows]: [Organization[], number] = await manager.query(
          `UPDATE public.organizations SET code = $2, name = $3, enabled = $4, updated_at = CURRENT_TIMESTAMP
          WHERE id = $1 RETURNING id, code, name, enabled`, [id, input.code, input.name, input.enabled],
        );
        return rows;
      });
      await this.identity.audit(manager, 'organization_updated', requestId, actor.id, undefined, null,
        { name: org.name, enabled: org.enabled });
      return { org, revoked: before.enabled !== org.enabled };
    }, result => {
      if (result.revoked) this.identity.revokeOrganization(id);
    }).then(result => result.org);
  }

  async members(token: string | undefined, input: MemberQuery & { limit: number; offset: number }) {
    const { organizationId } = await this.identity.requireOrganizationAdmin(this.manager, token);
    const rows = await organizations.members(this.manager, organizationId, input);
    await this.identity.requireOrganizationAdmin(this.manager, token);
    return rows;
  }

  async candidate(token: string | undefined, username: string): Promise<MemberCandidate> {
    const { organizationId } = await this.identity.requireOrganizationAdmin(this.manager, token, true);
    const [row]: MemberCandidate[] = await this.manager.query(`SELECT id, username, display_name, enabled
      FROM public.users u WHERE username = $1 AND NOT EXISTS (
        SELECT 1 FROM public.organization_members m WHERE m.organization_id = $2 AND m.user_id = u.id
      )`, [username, organizationId]);
    await this.identity.requireOrganizationAdmin(this.manager, token, true);
    if (!row) throw new IdentityError(404, 'USER_NOT_FOUND');
    return row;
  }

  async updateSystemMember(token: string | undefined, organizationId: string, userId: number, patch: UpdateMember, requestId: string) {
    const result = await this.writeMember(token, userId, { kind: 'update', organizationId, patch }, requestId);
    return result.member!;
  }

  async addSystemMember(token: string | undefined, organizationId: string, userId: number, input: SaveMember, requestId: string) {
    await this.writeMember(token, userId, { kind: 'add', organizationId, input }, requestId);
    return input;
  }

  removeSystemMember(token: string | undefined, organizationId: string, userId: number, requestId: string) {
    return this.writeMember(token, userId, { kind: 'remove', organizationId }, requestId);
  }

  changeMember(token: string | undefined, userId: number, input: SaveMember | undefined, requestId: string) {
    return this.writeMember(token, userId, { kind: 'organization', input }, requestId);
  }

  private writeMember(token: string | undefined, userId: number, change: MemberChange, requestId: string) {
    return this.identity.write(async manager => {
      const { user: actor, organizationId } = change.kind === 'organization'
        ? await this.identity.requireOrganizationAdmin(manager, token, true)
        : { user: await this.identity.requireAdmin(manager, token), organizationId: change.organizationId };
      const user = await users.byId(manager, userId);
      if (!user) throw new IdentityError(404, 'USER_NOT_FOUND');
      const org = await organizations.get(manager, organizationId);
      if (change.kind !== 'organization' && !org) throw new IdentityError(404, 'ORGANIZATION_NOT_FOUND');
      const before = await organizations.member(manager, organizationId, userId);
      let input: SaveMember | undefined;
      switch (change.kind) {
        case 'add':
          if (before && (before.role !== change.input.role || before.enabled !== change.input.enabled))
            throw new IdentityError(409, 'ORGANIZATION_MEMBER_EXISTS');
          if (!before && !org?.enabled) throw new IdentityError(409, 'ORGANIZATION_DISABLED');
          input = change.input;
          break;
        case 'update':
          if (!before) throw new IdentityError(404, 'ORGANIZATION_MEMBER_NOT_FOUND');
          input = { role: change.patch.role ?? before.role, enabled: change.patch.enabled ?? before.enabled };
          break;
        case 'organization':
          input = change.input;
          break;
        case 'remove':
          input = undefined;
          break;
      }
      if ((!before && !input) || (before && input && before.role === input.role && before.enabled === input.enabled))
        return { member: input };
      if (org?.enabled && user.enabled && before?.enabled && before.role === 'admin' &&
        !(input?.enabled && input.role === 'admin') && !await organizations.hasAdmin(manager, organizationId, userId))
        throw new IdentityError(409, 'ORGANIZATION_ADMIN_REQUIRED');
      if (input?.enabled === false) {
        await manager.query('DELETE FROM public.host_device_users WHERE organization_id = $1 AND user_id = $2',
          [organizationId, userId]);
      }
      if (input) await organizations.saveMember(manager, organizationId, userId, input);
      else await manager.query('DELETE FROM public.organization_members WHERE organization_id = $1 AND user_id = $2',
        [organizationId, userId]);
      await this.identity.audit(manager, input ? 'organization_member_saved' : 'organization_member_removed',
        requestId, actor.id, undefined, userId, input ? { enabled: input.enabled } : {}, organizationId);
      return { member: input, revokedOrganizationId: before?.enabled && !input?.enabled ? organizationId : undefined };
    }, result => {
      if (result.revokedOrganizationId) this.identity.revokeOrganization(result.revokedOrganizationId, userId);
    });
  }
}
