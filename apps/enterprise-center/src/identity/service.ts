import { randomBytes } from 'node:crypto';
import type { DataSource, EntityManager } from 'typeorm';
import type {
  LoginResponse,
  IdentityResponse,
  IdentityUser,
  LoginRequest,
  ScopeView,
  PasswordRequest,
} from '../contracts/identity.js';
import { IdentityError } from './errors.js';
import { Passwords } from './password.js';
import { LoginLimiter } from './validation.js';
import { CenterIdentityEntity } from './entity.js';
import { users } from '../users/repository.js';
import { audits } from '../audit/repository.js';
import { organizations } from '../organizations/repository.js';
import { CenterTokens, sameScope } from './token.js';
import type { Scope } from './token.js';

function publicUser(user: IdentityUser): IdentityUser {
  return {
    id: user.id,
    username: user.username,
    display_name: user.display_name,
    role: user.role,
    is_super_admin: user.is_super_admin,
    enabled: user.enabled,
  };
}

export type LoginScope = Readonly<{
  jti: string;
  userId: number;
  issuer: string;
  scope: Scope;
  expires: number;
  llmKey?: string;
}>;

export function isSystemAdmin(user: IdentityUser): boolean {
  return user.is_super_admin || user.role === 'system_admin';
}

type Entry = 'admin' | 'client';
type PreparedLogin = { login: LoginResponse; session: LoginScope };

/** 单实例进程拥有 Token；私有签名密钥与有效会话不持久化，关闭或重启全部失效。 */
export class IdentityService {
  readonly passwords = new Passwords();
  readonly limiter = new LoginLimiter();
  private readonly jwt = new CenterTokens();
  private expiry?: NodeJS.Timeout;
  private readonly tokens = new Map<string, LoginScope>();
  private readonly llmKeys = new Map<string, string>();
  private readonly organizationRevoked = new Set<(id: string) => void>();
  private readonly sessionRevoked = new Set<(scope: LoginScope) => void>();
  private pendingWrite: Promise<void> = Promise.resolve();
  private readonly revoked = new Set<(user: number | null) => void>();

  constructor(private readonly connection?: DataSource) {}

  private get source(): DataSource {
    if (!this.connection?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.connection;
  }

  onModuleDestroy(): void {
    for (const jti of this.tokens.keys()) this.revoke(jti);
    clearTimeout(this.expiry);
    for (const notify of this.revoked) notify(null);
  }

  /** 传输 owner 订阅已提交的主体撤销；不把凭据或登录 Token 暴露给资源监听方。 */
  onRevoked(notify: (user: number | null) => void): () => void {
    this.revoked.add(notify);
    return () => {
      this.revoked.delete(notify);
    };
  }

  onSessionRevoked(notify: (scope: LoginScope) => void): () => void {
    this.sessionRevoked.add(notify);
    return () => { this.sessionRevoked.delete(notify); };
  }
  onOrganizationRevoked(notify: (id: string) => void): () => void {
    this.organizationRevoked.add(notify);
    return () => { this.organizationRevoked.delete(notify); };
  }

  async scope(token?: string): Promise<LoginScope> {
    const claims = await this.jwt.verify(token);
    const session = this.tokens.get(claims.jti);
    if (!session || session.userId !== claims.userId || session.issuer !== claims.issuer ||
      session.expires !== claims.expires || !sameScope(session.scope, claims.scope))
      throw new IdentityError(401, 'TOKEN_INVALID');
    this.assertActive(session);
    return session;
  }

  private assertActive(session: LoginScope) {
    if (this.tokens.get(session.jti) !== session || session.expires * 1000 <= Date.now())
      throw new IdentityError(401, 'TOKEN_INVALID');
  }

  async prepareLogin(
    client: EntityManager, user: IdentityUser, requestId: string, action = 'login',
    scope: Scope = { kind: 'unselected' }, issueKey = false,
  ): Promise<PreparedLogin> {
    const center = await client.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true });
    const view = await this.scopeView(client, user, scope);
    const { token, jti, expires } = await this.jwt.issue(center.center_id, user.id, scope);
    const llmKey = issueKey ? `cl_${randomBytes(32).toString('hex')}` : undefined;
    const session = { jti, expires, issuer: center.center_id, userId: user.id, scope, llmKey };
    await this.audit(client, action, requestId, user.id, undefined, user.id, {},
      scope.kind === 'organization' ? scope.organization_id : undefined);
    return { session, login: this.loginResponse(center, user, view, session, token) };
  }

  private loginResponse(center: CenterIdentityEntity, user: IdentityUser, scope: ScopeView, session: LoginScope, token: string): LoginResponse {
    return {
      center_id: center.center_id, legacy_organization_id: center.legacy_organization_id, user: publicUser(user),
      scope, expires_at: new Date(session.expires * 1000).toISOString(), token,
      ...(session.llmKey ? { llm_key: session.llmKey } : {}),
    };
  }

  publishLogin({ session }: PreparedLogin) {
    this.tokens.set(session.jti, session);
    if (session.llmKey) this.llmKeys.set(session.llmKey, session.jti);
    this.scheduleExpiry();
  }

  private scheduleExpiry() {
    clearTimeout(this.expiry);
    let nearest = Infinity;
    for (const session of this.tokens.values()) nearest = Math.min(nearest, session.expires * 1000);
    if (!Number.isFinite(nearest)) return;
    this.expiry = setTimeout(() => {
      const now = Date.now();
      for (const [jti, session] of this.tokens) if (session.expires * 1000 <= now) this.revoke(jti);
      this.scheduleExpiry();
    }, Math.max(0, nearest - Date.now())).unref();
  }

  /** 串行化身份写入直到提交后的内存变更完成，避免退出/改密/登录交错复活旧凭据。
   * 密码计算在队列外；此边界只支持单实例，不模拟跨进程同步。提交结果不明时清空凭据，禁止重试。
   */
  write<T>(work: (manager: EntityManager) => Promise<T>, committed: (result: T) => void = () => {}): Promise<T> {
    const pending = this.pendingWrite.then(async () => {
      const source = this.source,
        runner = source.createQueryRunner();
      let committing = false;
      try {
        await runner.connect();
        await runner.startTransaction();
        const result = await work(runner.manager);
        committing = true;
        await runner.commitTransaction();
        committed(result);
        return result;
      } catch (error) {
        if (committing) this.onModuleDestroy();
        try {
          if (runner.isTransactionActive) await runner.rollbackTransaction();
        } catch {
          // 无法确认回滚时关闭业务池，禁止不明事务连接回池；需要重启恢复，不重试写入。
          this.onModuleDestroy();
          try {
            await source.destroy();
          } catch {
            /* 保留原始失败，对外由全局错误层脱敏。 */
          }
        }
        throw error;
      } finally {
        await runner.release();
      }
    });
    this.pendingWrite = pending.then(
      () => {},
      () => {},
    );
    return pending;
  }

  async audit(
    manager: EntityManager,
    action: string,
    requestId: string,
    userId: number | null,
    reason?: string,
    targetId = userId,
    details: object = {},
    organizationId?: string,
  ): Promise<void> {
    await audits.append(manager, action, requestId, userId, reason, targetId, details, organizationId);
  }

  /** 拒绝审计只含固定动作/原因码，不复制请求字段；审计故障不伪称记录成功。 */
  async attempt<T>(action: string, requestId: string, work: () => Promise<T>, actorId?: number, organizationId?: string): Promise<T> {
    try {
      return await work();
    } catch (error) {
      await this.rejection(action, requestId, error, actorId, organizationId);
      throw error;
    }
  }

  async rejection(action: string, requestId: string, error: unknown, actorId?: number, organizationId?: string): Promise<void> {
    if (error instanceof IdentityError && error.status < 500) {
      await this.audit(this.source.manager, action, requestId, actorId ?? null, error.code, null, {}, organizationId);
    }
  }

  /** 管理写入在共用串行段内重验范围和权限。 */
  async requireAdmin(client: EntityManager, token?: string): Promise<IdentityUser> {
    const session = await this.scope(token);
    const user = await this.account(client, session);
    if (session.scope.kind !== 'system' || !isSystemAdmin(user)) throw new IdentityError(403, 'ADMIN_REQUIRED');
    return publicUser(user);
  }

  async requireOrganizationAdmin(client: EntityManager, token?: string, systemOnly = false) {
    const session = await this.scope(token);
    const user = await this.account(client, session);
    const view = await this.scopeView(client, user, session.scope);
    this.assertActive(session);
    if (session.scope.kind !== 'organization' || !view.can_manage || (systemOnly && !isSystemAdmin(user)))
      throw new IdentityError(403, 'SCOPE_DENIED');
    return { organizationId: session.scope.organization_id, user: publicUser(user), session };
  }

  async requireOrganizationMember(client: EntityManager, token?: string) {
    const session = await this.scope(token);
    const user = await this.account(client, session);
    const view = await this.scopeView(client, user, session.scope);
    this.assertActive(session);
    if (session.scope.kind !== 'organization' || !view.member_role)
      throw new IdentityError(403, 'SCOPE_DENIED');
    return { organizationId: session.scope.organization_id, user: publicUser(user), session };
  }

  async requireUser(client: EntityManager, token?: string): Promise<IdentityUser> {
    return publicUser(await this.userForToken(client, token));
  }

  revokeUser(id: number): void {
    for (const [jti, session] of this.tokens) if (session.userId === id) this.revoke(jti);
    this.scheduleExpiry();
    for (const notify of this.revoked) notify(id);
  }

  revokeOrganization(id: string, userId?: number): void {
    for (const [jti, session] of this.tokens) {
      if (session.scope.kind === 'organization' && session.scope.organization_id === id &&
        (userId === undefined || userId === session.userId)) this.revoke(jti);
    }
    if (userId === undefined) for (const notify of this.organizationRevoked) notify(id);
    this.scheduleExpiry();
  }

  private async account(client: EntityManager, session: LoginScope) {
    const row = await users.forIdentity(client, session.userId);
    this.assertActive(session);
    if (!row?.enabled) throw new IdentityError(401, 'TOKEN_INVALID');
    return row;
  }

  private async userForToken(client: EntityManager, token?: string) {
    return this.account(client, await this.scope(token));
  }

  private async scopeView(client: EntityManager, user: IdentityUser, scope: Scope): Promise<ScopeView> {
    if (scope.kind === 'unselected') return { ...scope, can_manage: false };
    if (scope.kind === 'system') {
      if (!isSystemAdmin(user)) throw new IdentityError(403, 'SCOPE_DENIED');
      return { ...scope, can_manage: true };
    }
    const org = await organizations.get(client, scope.organization_id);
    if (!org) throw new IdentityError(403, 'SCOPE_DENIED');
    const member = await organizations.member(client, org.id, user.id);
    const memberRole = org.enabled && member?.enabled ? member.role : undefined;
    if (!memberRole && !isSystemAdmin(user)) throw new IdentityError(403, 'SCOPE_DENIED');
    return { ...scope, name: org.name, code: org.code, enabled: org.enabled,
      ...(memberRole ? { member_role: memberRole } : {}),
      can_manage: isSystemAdmin(user) || memberRole === 'admin' };
  }

  async scopes(token: string | undefined, entry: Entry): Promise<ScopeView[]> {
    const client = this.source.manager;
    const session = await this.scope(token);
    const user = await this.account(client, session);
    const members = await organizations.forAccount(client, user.id);
    const system = entry === 'admin' && isSystemAdmin(user);
    const candidates = system ? await organizations.list(client) : members;
    this.assertActive(session);
    const roles = new Map(members.map(org => [org.id, org.member_role]));
    const result: ScopeView[] = system ? [{ kind: 'system', can_manage: true }] : [];
    for (const org of candidates) {
      const memberRole = roles.get(org.id);
      if (entry === 'admin' && !system && memberRole !== 'admin') continue;
      result.push({ kind: 'organization', organization_id: org.id, name: org.name, code: org.code, enabled: org.enabled,
        ...(memberRole ? { member_role: memberRole } : {}), can_manage: isSystemAdmin(user) || memberRole === 'admin' });
    }
    return result;
  }

  async exchange(token: string | undefined, target: Scope, entry: Entry, requestId: string): Promise<LoginResponse> {
    if (target.kind === 'unselected') throw new IdentityError(400, 'INVALID_REQUEST');
    const result = await this.write(async client => {
      const old = await this.scope(token);
      const user = await this.account(client, old);
      const view = await this.scopeView(client, user, target);
      if (entry === 'admin' ? !view.can_manage : !view.member_role) throw new IdentityError(403, 'SCOPE_DENIED');
      if (sameScope(old.scope, target)) {
        const center = await client.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true });
        this.assertActive(old);
        return { old, login: this.loginResponse(center, user, view, old, token!) };
      }
      const next = await this.prepareLogin(client, user, requestId, 'scope_exchanged', target, entry === 'client');
      this.assertActive(old);
      return { old, next, login: next.login };
    }, ({ old, next }) => {
      if (next && this.tokens.get(old.jti) === old && old.expires * 1000 > Date.now()) {
        this.publishLogin(next);
        this.revoke(old.jti);
      }
    });
    this.assertActive(result.next?.session ?? result.old);
    return result.login;
  }

  async login(input: LoginRequest, requestId: string): Promise<LoginResponse> {
    const before = await users.byName(this.source.manager, input.username);
    const matches = await this.passwords.verify(input.password, before?.password_hash);
    if (!matches || !before?.enabled) throw new IdentityError(401, 'AUTH_FAILED');
    const result = await this.write(
      async (client) => {
        const user = await users.byName(client, input.username);
        if (!user?.enabled) throw new IdentityError(401, 'AUTH_FAILED');
        if (
          user.password_hash !== before.password_hash ||
          user.role !== before.role ||
          user.is_super_admin !== before.is_super_admin
        ) {
          throw new IdentityError(409, 'AUTH_STATE_CHANGED');
        }
        return this.prepareLogin(client, user, requestId);
      },
      (result) => this.publishLogin(result),
    );
    return result.login;
  }

  /** 中心身份与访问地址无关；公开发现与已认证身份使用同一持久事实。 */
  async centerId(): Promise<string> {
    const center = await this.source.getRepository(CenterIdentityEntity).findOneBy({ singleton: true });
    if (!center) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return center.center_id;
  }

  async me(token?: string): Promise<IdentityResponse> {
    const client = this.source.manager;
    const session = await this.scope(token);
    const user = await this.account(client, session);
    const scope = await this.scopeView(client, user, session.scope).catch((error: unknown) => {
      if (error instanceof IdentityError && error.code === 'SCOPE_DENIED') throw new IdentityError(401, 'TOKEN_INVALID');
      throw error;
    });
    const center = await client.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true });
    this.assertActive(session);
    return { center_id: center.center_id, legacy_organization_id: center.legacy_organization_id, user: publicUser(user),
      scope, expires_at: new Date(session.expires * 1000).toISOString() };
  }

  /** LLM key 只关联 jti，独立校验原组织的有效成员资格。 */
  async llmIdentity(key: string): Promise<IdentityResponse & { session: LoginScope }> {
    const jti = this.llmKeys.get(key);
    const session = jti ? this.tokens.get(jti) : undefined;
    if (!session) throw new IdentityError(401, 'TOKEN_INVALID');
    const client = this.source.manager;
    const user = await this.account(client, session);
    const scope = await this.scopeView(client, user, session.scope).catch((error: unknown) => {
      if (error instanceof IdentityError && error.code === 'SCOPE_DENIED') throw new IdentityError(401, 'TOKEN_INVALID');
      throw error;
    });
    const center = await client.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true });
    this.assertActive(session);
    if (!scope.member_role) throw new IdentityError(401, 'TOKEN_INVALID');
    return { center_id: center.center_id, legacy_organization_id: center.legacy_organization_id,
      user: publicUser(user), scope, expires_at: new Date(session.expires * 1000).toISOString(), session };
  }

  discardSession(session: LoginScope): void {
    if (this.tokens.get(session.jti) === session) {
      this.revoke(session.jti);
      this.scheduleExpiry();
    }
  }

  private revoke(jti: string): void {
    const session = this.tokens.get(jti);
    if (!session) return;
    this.tokens.delete(jti);
    if (session.llmKey) this.llmKeys.delete(session.llmKey);
    for (const notify of this.sessionRevoked) notify(session);
  }

  async logout(token: string | undefined, requestId: string): Promise<void> {
    await this.write(async client => {
      const session = await this.scope(token).catch(() => undefined);
      if (session) await this.audit(client, 'logout', requestId, session.userId, undefined, session.userId, {},
        session.scope.kind === 'organization' ? session.scope.organization_id : undefined);
      return session;
    }, session => {
      if (session) this.revoke(session.jti);
      this.scheduleExpiry();
    });
  }

  async changePassword(token: string | undefined, input: PasswordRequest, requestId: string): Promise<void> {
    const before = await this.userForToken(this.source.manager, token);
    if (!(await this.passwords.verify(input.old_password, before.password_hash)))
      throw new IdentityError(401, 'AUTH_FAILED');
    const hash = await this.passwords.hash(input.new_password);
    await this.write(async (client) => {
      const current = await this.userForToken(client, token);
      if (
        current.password_hash !== before.password_hash ||
        current.role !== before.role ||
        current.is_super_admin !== before.is_super_admin
      ) {
        throw new IdentityError(409, 'AUTH_STATE_CHANGED');
      }
      await users.password(client, current.id, hash);
      await this.audit(client, 'self_password_changed', requestId, current.id);
    });
  }
}
