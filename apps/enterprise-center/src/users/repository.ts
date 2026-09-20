import type { EntityManager } from 'typeorm';
import { QueryFailedError } from 'typeorm';
import { UserEntity } from './entity.js';
import { IdentityError } from '../identity/errors.js';
import type { CreateUserRequest, UserRecord } from '../contracts/users.js';
import type { userQuery } from './validation.js';

const columns = ['id', 'username', 'display_name', 'role', 'is_super_admin', 'enabled', 'created_at', 'updated_at'];
type UserRow = Omit<UserRecord, 'created_at' | 'updated_at'> & { created_at: Date; updated_at: Date };

/** 无独立连接/事务状态，始终使用调用者传入的 Manager，避免审计和业务分属两条连接。 */
export const users = {
  byName(manager: EntityManager, username: string) {
    return manager.getRepository(UserEntity).createQueryBuilder('u').addSelect('u.password_hash')
      .where('u.username = :username', { username }).getOne();
  },
  byId(manager: EntityManager, id: number) { return manager.getRepository(UserEntity).findOneBy({ id }); },
  forIdentity(manager: EntityManager, id: number) {
    return manager.getRepository(UserEntity).createQueryBuilder('u').addSelect('u.password_hash')
      .where('u.id = :id', { id }).getOne();
  },
  async create(manager: EntityManager, input: CreateUserRequest, password_hash: string) {
    try {
      const result = await manager.createQueryBuilder().insert().into(UserEntity).values({
        username: input.username, display_name: input.display_name, role: input.role, enabled: input.enabled, password_hash })
        .returning(columns).execute();
      return (result.raw as UserRow[])[0]!;
    } catch (error) {
      // 只把已知用户名唯一约束转换成业务冲突；事务由上层回滚，其他数据库错误不泄露。
      if (error instanceof QueryFailedError && Reflect.get(error.driverError, 'code') === '23505'
        && Reflect.get(error.driverError, 'constraint') === 'users_username_key') throw new IdentityError(409, 'USERNAME_EXISTS');
      throw error;
    }
  },
  async update(manager: EntityManager, id: number, input: Pick<UserRecord, 'display_name' | 'role' | 'enabled'>) {
    const result = await manager.createQueryBuilder().update(UserEntity).set({ display_name: input.display_name,
      role: input.role, enabled: input.enabled, updated_at: () => 'CURRENT_TIMESTAMP' })
      .where('id = :id', { id }).returning(columns).execute();
    return (result.raw as UserRow[])[0];
  },
  async password(manager: EntityManager, id: number, password_hash: string) {
    await manager.getRepository(UserEntity).update({ id }, { password_hash, updated_at: () => 'CURRENT_TIMESTAMP' });
  },
  otherAdmins(manager: EntityManager, id: number) {
    return manager.getRepository(UserEntity).createQueryBuilder('u').where('u.enabled = true')
      .andWhere('(u.role = :role OR u.is_super_admin = true)', { role: 'admin' }).andWhere('u.id <> :id', { id }).getCount();
  },
  async list(manager: EntityManager, input: ReturnType<typeof userQuery>) {
    // 单语句保证列表/总数同一快照（包括空页）；复杂 CTE 保留参数化 SQL，不强行拆成两个 ORM 查询。
    const rows: { items: UserRecord[]; total: number }[] = await manager.query(`WITH filtered AS (
      SELECT ${columns.join(',')} FROM public.users WHERE
      ($1::text IS NULL OR strpos(lower(username),lower($1))>0 OR strpos(lower(display_name),lower($1))>0)
      AND ($2::text IS NULL OR role=$2) AND ($3::boolean IS NULL OR enabled=$3)
    ), page AS (SELECT * FROM filtered ORDER BY created_at,id LIMIT $4 OFFSET $5)
    SELECT (SELECT count(*)::integer FROM filtered) AS total,
      COALESCE((SELECT json_agg(page ORDER BY created_at,id) FROM page),'[]'::json) AS items`,
    [input.search ?? null, input.role ?? null, input.enabled ?? null, input.limit, input.offset]);
    return rows[0]!;
  },
};
