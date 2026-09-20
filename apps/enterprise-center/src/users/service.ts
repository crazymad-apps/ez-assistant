import type { DataSource, EntityManager } from 'typeorm';
import type { CreateUserRequest, UpdateUserRequest, UserPage, UserRecord } from '../contracts/users.js';
import { IdentityError } from '../identity/errors.js';
import type { IdentityService } from '../identity/service.js';
import type { userQuery } from './validation.js';
import { users } from './repository.js';

type UserRow = Omit<UserRecord, 'created_at' | 'updated_at'> & { created_at: Date; updated_at: Date };
const response = (row: UserRow): UserRecord => ({ id: row.id, username: row.username, display_name: row.display_name,
  role: row.role, is_super_admin: row.is_super_admin, enabled: row.enabled,
  created_at: row.created_at.toISOString(), updated_at: row.updated_at.toISOString() });
const details = (row: Pick<UserRecord, 'display_name' | 'role' | 'enabled'>) => ({ display_name: row.display_name, role: row.role, enabled: row.enabled });

/** 此层只编排规则；数据访问归 repository，事务、权限重验与 Token 撤销复用同一 IdentityService。 */
export class UsersService {
  constructor(private readonly identity: IdentityService, private readonly connection?: DataSource) {}
  private get manager(): EntityManager { if (!this.connection) throw new IdentityError(503, 'SERVICE_UNAVAILABLE'); return this.connection.manager; }
  private async target(client: EntityManager, id: number): Promise<UserRow> {
    const row = await users.byId(client, id);
    if (!row) throw new IdentityError(404, 'USER_NOT_FOUND'); return row;
  }

  async list(input: ReturnType<typeof userQuery>): Promise<UserPage> {
    const row = await users.list(this.manager, input);
    return { ...row, items: row.items.map(item => ({ ...item, created_at: new Date(item.created_at).toISOString(),
      updated_at: new Date(item.updated_at).toISOString() })), limit: input.limit, offset: input.offset };
  }

  async create(token: string | undefined, input: CreateUserRequest, requestId: string): Promise<UserRecord> {
    await this.identity.requireAdmin(this.manager, token);
    const hash = await this.identity.passwords.hash(input.password);
    return this.identity.write(async client => {
      const actor = await this.identity.requireAdmin(client, token);
      const row = await users.create(client, input, hash);
      await this.identity.audit(client, 'user_created', requestId, actor.id, undefined, row.id, { after: details(row) });
      return response(row);
    });
  }

  async update(token: string | undefined, id: number, input: UpdateUserRequest, requestId: string): Promise<UserRecord> {
    const result = await this.identity.write(async client => {
      const actor = await this.identity.requireAdmin(client, token);
      const before = await this.target(client, id), after = { ...before, ...input };
      // 只认标记，不认用户名、固定 ID 或“唯一超级管理员”；数据库赋予的标记同样受保护。
      if (before.is_super_admin && (input.enabled === false || (before.role !== 'user' && input.role === 'user'))) {
        throw new IdentityError(409, 'SUPER_ADMIN_PROTECTED');
      }
      const wasAdmin = before.enabled && (before.role === 'admin' || before.is_super_admin);
      const remainsAdmin = after.enabled && (after.role === 'admin' || after.is_super_admin);
      if (wasAdmin && !remainsAdmin) {
        const count = await users.otherAdmins(client, id);
        if (!count) throw new IdentityError(409, 'LAST_ADMIN_REQUIRED');
      }
      const changed = before.display_name !== after.display_name || before.role !== after.role || before.enabled !== after.enabled;
      if (!changed) return { user: response(before), revoke: false };
      const row = (await users.update(client, id, after))!;
      await this.identity.audit(client, 'user_updated', requestId, actor.id, undefined, id, { before: details(before), after: details(row) });
      return { user: response(row), revoke: before.role !== row.role || before.enabled !== row.enabled };
    }, result => { if (result.revoke) this.identity.revokeUser(id); });
    return result.user;
  }

  async resetPassword(token: string | undefined, id: number, password: string, requestId: string): Promise<void> {
    await this.identity.requireAdmin(this.manager, token);
    const hash = await this.identity.passwords.hash(password);
    await this.identity.write(async client => {
      const actor = await this.identity.requireAdmin(client, token);
      await this.target(client, id);
      await users.password(client, id, hash);
      // actor=target 不改变管理重置语义，也不豁免个人改密接口的原密码检查。
      await this.identity.audit(client, 'user_password_reset', requestId, actor.id, undefined, id);
    }, () => this.identity.revokeUser(id));
  }
}
