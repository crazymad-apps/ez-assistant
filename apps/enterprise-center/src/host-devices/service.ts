import { createHash, timingSafeEqual } from 'node:crypto';
import { isDeepStrictEqual } from 'node:util';
import { In } from 'typeorm';
import type { EntityManager } from 'typeorm';
import type { HostDeviceDetail, HostDeviceState, HostDeviceSummary } from '../contracts/host-devices.js';
import { CenterIdentityEntity } from '../identity/entity.js';
import { IdentityError } from '../identity/errors.js';
import { IdentityService } from '../identity/service.js';
import { organizations } from '../organizations/repository.js';
import { UserEntity } from '../users/entity.js';
import { fields, invalid, page, queryFields } from '../validation.js';
import { HostDeviceEntity, HostDeviceUserEntity } from './entity.js';
import { application, deviceId, secret, state } from './validation.js';

const digest = (value: string) => createHash('sha256').update(value).digest('hex');

function summary(row: HostDeviceEntity, online = false): HostDeviceSummary {
  return {
    id: row.id,
    organization_id: row.organization_id,
    installation_id: row.installation_id,
    name: row.name,
    platform: row.platform,
    host_version: row.host_version,
    applicant_user_id: row.applicant_user_id,
    status: row.status,
    enabled: row.enabled,
    online,
    created_at: row.created_at.toISOString(),
    updated_at: row.updated_at.toISOString(),
  };
}

async function detail(manager: EntityManager, row: HostDeviceEntity, online = false): Promise<HostDeviceDetail> {
  const users = await manager
    .getRepository(HostDeviceUserEntity)
    .find({ where: { device_id: row.id }, order: { user_id: 'ASC' } });
  return {
    ...summary(row, online),
    assigned_users: users.length ? (await manager.getRepository(UserEntity).findBy({ id: In(users.map(user => user.user_id)) }))
      .map(({ id, username, display_name, role, is_super_admin, enabled }) => ({ id, username, display_name, role, is_super_admin, enabled })) : [],
    users: users.map(({ user_id, desktop_view, desktop_control }) => ({ user_id, desktop_view, desktop_control })),
  };
}

function editable(value: HostDeviceDetail): HostDeviceState {
  return { status: value.status, name: value.name, enabled: value.enabled, users: value.users };
}

async function centerId(manager: EntityManager): Promise<string> {
  return (await manager.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true })).center_id;
}
export type DeviceConnectionScope = {
  center: string;
  organizationId: string;
  session?: import('../identity/service.js').LoginScope;
};

/** 与用户管理共用事务顺序，审批/用户删除/授权替换不会交错发布部分事实。 */
export class HostDevicesService {
  private connection?: { online(id: string): boolean; onlineIds(): string[]; changed(id: string, terminal: boolean, device?: HostDeviceDetail): void };
  constructor(private readonly identity: IdentityService) {}

  attachConnection(connection: NonNullable<HostDevicesService['connection']>) {
    this.connection = connection;
  }

  /** 凭据核验与登记变更共用串行事务；接纳回调只发布内存状态，不等待网络。 */
  async admitCaller(token: string, accepted: (scope: DeviceConnectionScope) => void) {
    return this.identity.write(async (manager) => {
      const { organizationId, session } = await this.identity.requireOrganizationMember(manager, token);
      return { center: await centerId(manager), organizationId, session };
    }, accepted);
  }
  async admit(id: string, token: string, accepted: (scope: DeviceConnectionScope) => void) {
    const key = deviceId(id);
    if (!/^[0-9a-f]{64}$/.test(token)) throw new IdentityError(401, 'DEVICE_CREDENTIAL_INVALID');
    return this.identity.write(async (manager) => {
      const row = await manager.getRepository(HostDeviceEntity).createQueryBuilder('d')
        .addSelect('d.credential_hash').where('d.id = :id', { id: key }).getOne();
      if (!row || !this.matches(row, token)) throw new IdentityError(401, 'DEVICE_CREDENTIAL_INVALID');
      if (!['pending', 'registered'].includes(row.status)) throw new IdentityError(410, 'DEVICE_CHANGED');
      if (!(await organizations.get(manager, row.organization_id))?.enabled)
        throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
      return { center: await centerId(manager), organizationId: row.organization_id };
    }, accepted);
  }

  private summary(row: HostDeviceEntity) {
    return summary(row, ['pending', 'registered'].includes(row.status) && (this.connection?.online(row.id) ?? false));
  }
  private detail(manager: EntityManager, row: HostDeviceEntity) {
    return detail(manager, row, ['pending', 'registered'].includes(row.status) && (this.connection?.online(row.id) ?? false));
  }
  private changed(device: HostDeviceSummary | null) {
    if (device) this.connection?.changed(device.id, !['pending', 'registered'].includes(device.status), 'users' in device ? device as HostDeviceDetail : undefined);
  }

  async directory(manager: EntityManager, organizationId: string, user: number, offset: number) {
    const grants = await manager.getRepository(HostDeviceUserEntity).find({ where: { organization_id: organizationId, user_id: user }, order: { device_id: 'ASC' }, skip: offset, take: 20 });
    const rows = grants.length ? await manager.getRepository(HostDeviceEntity).find({ where: { id: In(grants.map((g) => g.device_id)), organization_id: organizationId, status: 'registered' }, order: { id: 'ASC' } }) : [];
    return { items: rows.map((row) => {
      const grant = grants.find((g) => g.device_id === row.id)!;
      return { id: row.id, name: row.name, platform: row.platform, enabled: row.enabled,
        online: this.connection?.online(row.id) ?? false, desktop_view: grant.desktop_view, desktop_control: grant.desktop_control };
    }), next_offset: grants.length === 20 ? offset + 20 : null };
  }
  async allow(manager: EntityManager, organizationId: string, user: number, id: string, capability: string) {
    const row = await manager.getRepository(HostDeviceEntity).findOneBy({ id, organization_id: organizationId });
    const grant = await manager.getRepository(HostDeviceUserEntity).findOneBy({ device_id: id, user_id: user, organization_id: organizationId });
    if (!row || row.status !== 'registered' || !row.enabled || !grant ||
        (capability === 'desktop_view' && !grant.desktop_view) ||
        (capability === 'desktop_control' && (!grant.desktop_view || !grant.desktop_control)))
      throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
  }

  async allowSource(manager: EntityManager, organizationId: string, id: string | null) {
    if (id === null) return;
    const source = await manager.getRepository(HostDeviceEntity).existsBy({
      id, organization_id: organizationId, enabled: true, status: 'registered',
    });
    if (!source) throw new IdentityError(403, 'DEVICE_ACCESS_DENIED');
  }

  async apply(token: string | undefined, value: unknown, requestId: string) {
    const input = application(value);
    return this.identity.write(async (manager) => {
      const { user: actor, organizationId } = await this.identity.requireOrganizationMember(manager, token);
      const center = await centerId(manager);
      if (input.center_id !== center) throw new IdentityError(409, 'DEVICE_CENTER_MISMATCH');
      const repo = manager.getRepository(HostDeviceEntity);
      const existing = await repo
        .createQueryBuilder('d')
        .addSelect('d.credential_hash')
        .where('d.id = :id', { id: input.id })
        .getOne();
      if (existing) {
        if (existing.organization_id !== organizationId || existing.installation_id !== input.installation_id || !this.matches(existing, input.secret))
          throw new IdentityError(409, 'DEVICE_CONFLICT');
        // 原名称可能已经由管理员修改；幂等重试只回原记录，不覆盖任何管理结果。
        return { center_id: center, device: this.summary(existing) };
      }
      if (await repo.existsBy({ installation_id: input.installation_id, status: In(['pending', 'registered']) }))
        throw new IdentityError(409, 'DEVICE_CONFLICT');
      await repo.insert({
        id: input.id,
        organization_id: organizationId,
        installation_id: input.installation_id,
        credential_hash: digest(input.secret),
        name: input.name,
        platform: input.platform,
        host_version: input.host_version,
        applicant_user_id: actor.id,
        status: 'pending',
        enabled: true,
      });
      await this.identity.audit(manager, 'device_applied', requestId, actor.id, undefined, actor.id, {
        device_id: input.id,
      }, organizationId);
      return { center_id: center, device: this.summary(await repo.findOneByOrFail({ id: input.id })) };
    });
  }

  /** 设备凭据仅用于本记录；不通过用户 Token 认证，不授予用户或管理员权限。 */
  async registration(id: string, token: string | undefined, action?: 'withdraw' | 'exit', requestId?: string) {
    const key = deviceId(id);
    if (!token || !/^[0-9a-f]{64}$/.test(token)) throw new IdentityError(401, 'DEVICE_CREDENTIAL_INVALID');
    return this.identity.write(async (manager) => {
      const repo = manager.getRepository(HostDeviceEntity);
      let row = await repo
        .createQueryBuilder('d')
        .addSelect('d.credential_hash')
        .where('d.id = :id', { id: key })
        .getOne();
      if (row && !this.matches(row, token)) throw new IdentityError(401, 'DEVICE_CREDENTIAL_INVALID');
      if (row && action) {
        if (action === 'withdraw' && row.status === 'registered') throw new IdentityError(409, 'DEVICE_CHANGED');
        if (row.status === 'pending' || row.status === 'registered') {
          const status = action === 'withdraw' ? 'withdrawn' : 'revoked';
          await repo.update(key, { status, updated_at: () => 'CURRENT_TIMESTAMP' });
          await manager.getRepository(HostDeviceUserEntity).delete({ device_id: key });
          await this.identity.audit(manager, `device_${status}`, requestId!, null, undefined, null, { device_id: key }, row.organization_id);
          row = await repo.findOneByOrFail({ id: key });
        }
      }
      return { center_id: await centerId(manager), device: row ? this.summary(row) : null };
    }, (reply) => { if (action) this.changed(reply.device); });
  }

  async end(id: string, token: string | undefined, value: unknown, requestId: string) {
    const { action } = fields(value, ['action']);
    if (action !== 'withdraw' && action !== 'exit') invalid();
    return this.registration(id, token, action, requestId);
  }

  async list(token: string | undefined, query: unknown) {
    const input = queryFields(query, ['status', 'limit', 'offset', 'search']);
    if ([...(input.search ?? '')].length > 80) invalid();
    if (input.status !== 'registered' && input.status !== 'unregistered') invalid();
    const paging = page(input);
    return this.identity.write(async (manager) => {
      const { organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
      const query = manager.getRepository(HostDeviceEntity).createQueryBuilder('d')
        .where('d.organization_id = :organizationId', { organizationId })
        .andWhere(input.status === 'registered' ? 'd.status = :registered' : 'd.status <> :registered', { registered: 'registered' });
      if (input.status === 'unregistered') query.andWhere("d.status = 'pending'");
      if (input.search?.trim()) query.andWhere('(strpos(lower(d.name), lower(:search)) > 0 OR strpos(d.id::text, :search) > 0)', { search: input.search.trim() });
      // 分页前将待审批申请排在未登记栏最前，避免后续页面的申请被历史记录遮住。
      const [rows, total] = await query.orderBy("CASE WHEN d.status = 'pending' THEN 0 ELSE 1 END", 'ASC')
        .addOrderBy('d.created_at', 'DESC').addOrderBy('d.id', 'ASC')
        .take(paging.limit).skip(paging.offset).getManyAndCount();
      const counts = rows.length ? await manager.getRepository(HostDeviceUserEntity).createQueryBuilder('g')
        .select('g.device_id', 'id').addSelect('COUNT(*)::int', 'count')
        .where('g.device_id IN (:...ids)', { ids: rows.map(row => row.id) }).groupBy('g.device_id')
        .getRawMany<{ id: string; count: number }>() : [];
      return { items: rows.map(row => ({ ...this.summary(row), user_count: counts.find(count => count.id === row.id)?.count ?? 0 })), total, ...paging };
    });
  }

  async get(token: string | undefined, id: string) {
    const key = deviceId(id);
    return this.identity.write(async (manager) => {
      const { organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
      const row = await manager.getRepository(HostDeviceEntity).findOneBy({ id: key, organization_id: organizationId });
      if (!row) throw new IdentityError(404, 'DEVICE_NOT_FOUND');
      return this.detail(manager, row);
    });
  }

  async manage(token: string | undefined, id: string, value: unknown, requestId: string) {
    const key = deviceId(id),
      input = fields(value, ['expected', 'desired']);
    const expected = state(input.expected),
      desired = state(input.desired);
    const approving = expected.status === 'pending' && desired.status === 'registered';
    if (!(
      approving ||
      (expected.status === 'pending' && desired.status === 'rejected') ||
      (expected.status === 'registered' && ['registered', 'revoked'].includes(desired.status))
    ))
      invalid();
    if (desired.status !== 'registered' && desired.users.length) invalid();
    return this.identity.write(async (manager) => {
      const { user: actor, organizationId } = await this.identity.requireOrganizationAdmin(manager, token);
      const repo = manager.getRepository(HostDeviceEntity),
        grants = manager.getRepository(HostDeviceUserEntity);
      const row = await repo.findOneBy({ id: key, organization_id: organizationId });
      if (!row) throw new IdentityError(404, 'DEVICE_NOT_FOUND');
      const current = await this.detail(manager, row);
      if ((approving && row.status === 'registered') || isDeepStrictEqual(editable(current), desired)) return current;
      if (!isDeepStrictEqual(editable(current), expected)) throw new IdentityError(409, 'DEVICE_CHANGED');
      if (desired.users.length) {
        const [assigned]: { count: number }[] = await manager.query(`
          SELECT count(*)::integer AS count FROM public.organization_members m
          JOIN public.users u ON u.id = m.user_id
          JOIN public.organizations o ON o.id = m.organization_id
          WHERE m.organization_id = $1 AND m.user_id = ANY($2::integer[]) AND m.enabled AND u.enabled AND o.enabled
        `, [organizationId, desired.users.map(user => user.user_id)]);
        if (assigned?.count !== desired.users.length) throw new IdentityError(409, 'DEVICE_CHANGED');
      }
      await repo.update(key, {
        name: desired.name,
        status: desired.status,
        enabled: desired.enabled,
        updated_at: () => 'CURRENT_TIMESTAMP',
      });
      await grants.delete({ device_id: key });
      if (desired.users.length) await grants.insert(desired.users.map((u) => ({ device_id: key, organization_id: organizationId, ...u })));
      await this.identity.audit(manager, 'device_managed', requestId, actor.id, undefined, null, {
        device_id: key,
        status: desired.status,
        enabled: desired.enabled,
        users: desired.users,
      }, organizationId);
      return this.detail(manager, await repo.findOneByOrFail({ id: key }));
    }, (device) => this.changed(device));
  }

  private matches(row: HostDeviceEntity, token: string) {
    return timingSafeEqual(Buffer.from(row.credential_hash, 'hex'), Buffer.from(digest(secret(token)), 'hex'));
  }
}
