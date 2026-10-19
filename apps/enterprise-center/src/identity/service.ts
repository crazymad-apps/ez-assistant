import { randomBytes } from 'node:crypto';
import type { DataSource, EntityManager } from 'typeorm';
import type {
  LoginResponse,
  IdentityResponse,
  IdentityUser,
  LoginRequest,
  PasswordRequest,
} from '../contracts/identity.js';
import { IdentityError } from './errors.js';
import { Passwords } from './password.js';
import { LoginLimiter } from './validation.js';
import { CenterIdentityEntity } from './entity.js';
import { users } from '../users/repository.js';
import { audits } from '../audit/repository.js';

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

export type LoginScope = Readonly<{ userId: number }>;

/** 单实例进程拥有 Token；不持久化、不计算摘要、不设 TTL，关闭或重启全部失效。 */
export class IdentityService {
  readonly passwords = new Passwords();
  readonly limiter = new LoginLimiter();
  private readonly tokens = new Map<string, LoginScope>();
  private readonly llmKeys = new Map<string, string>();
  private readonly sessionRevoked = new Set<(scope: LoginScope) => void>();
  private pendingWrite: Promise<void> = Promise.resolve();
  private readonly revoked = new Set<(user: number | null) => void>();

  constructor(private readonly connection?: DataSource) {}

  private get source(): DataSource {
    if (!this.connection?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.connection;
  }

  onModuleDestroy(): void {
    for (const scope of this.tokens.values()) for (const notify of this.sessionRevoked) notify(scope);
    this.tokens.clear();
    this.llmKeys.clear();
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
  scope(token: string): LoginScope {
    const scope = this.tokens.get(token);
    if (!scope) throw new IdentityError(401, 'TOKEN_INVALID');
    return scope;
  }
  /** 仅由已核实的组网访问在身份事务中调用；普通登录与受控签发共用同一凭据事实。 */
  async prepareLogin(client: EntityManager, user: IdentityUser, requestId: string, action = 'login'): Promise<LoginResponse> {
    const center = await client.getRepository(CenterIdentityEntity).findOneByOrFail({ singleton: true });
    const token = `ct_${randomBytes(32).toString('hex')}`, llm_key = `cl_${randomBytes(32).toString('hex')}`;
    await this.audit(client, action, requestId, user.id);
    return { center_id: center.center_id, user: publicUser(user), token, llm_key };
  }
  publishLogin(login: LoginResponse) {
    this.tokens.set(login.token, { userId: login.user.id });
    this.llmKeys.set(login.llm_key, login.token);
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
  ): Promise<void> {
    await audits.append(manager, action, requestId, userId, reason, targetId, details);
  }

  /** 拒绝审计只含固定动作/原因码，不复制请求字段；审计故障不伪称记录成功。 */
  async attempt<T>(action: string, requestId: string, work: () => Promise<T>, actorId?: number): Promise<T> {
    try {
      return await work();
    } catch (error) {
      await this.rejection(action, requestId, error, actorId);
      throw error;
    }
  }

  async rejection(action: string, requestId: string, error: unknown, actorId?: number): Promise<void> {
    if (error instanceof IdentityError && error.status < 500) {
      await this.audit(this.source.manager, action, requestId, actorId ?? null, error.code, null);
    }
  }

  /** 管理写入在共用串行段内重验；HTTP Guard 的旧快照不能授权异步计算后的提交。 */
  async requireAdmin(client: EntityManager, token?: string): Promise<IdentityUser> {
    const user = await this.userForToken(client, token);
    if (!user.is_super_admin && user.role !== 'admin') throw new IdentityError(403, 'ADMIN_REQUIRED');
    return publicUser(user);
  }

  /** 下载准入也须在短串行段内复查 Token 与用户启停状态。 */
  async requireUser(client: EntityManager, token?: string): Promise<IdentityUser> {
    return publicUser(await this.userForToken(client, token));
  }

  /** 仅在身份事务成功提交后调用；角色和启停变化撤销；改密与重置保留已有凭据。 */
  revokeUser(id: number): void {
    for (const [token, userId] of this.tokens) if (userId.userId === id) this.revoke(token);
    for (const notify of this.revoked) notify(id);
  }

  private async userForToken(client: EntityManager, token?: string) {
    const id = token && this.tokens.get(token)?.userId;
    if (!id) throw new IdentityError(401, 'TOKEN_INVALID');
    const row = await users.forIdentity(client, id);
    // 查询期间可能已经退出；不把异步查询前的认证结果当成仍然有效。
    if (!row?.enabled || this.tokens.get(token!)?.userId !== id) throw new IdentityError(401, 'TOKEN_INVALID');
    return row;
  }

  async login(input: LoginRequest, requestId: string): Promise<LoginResponse> {
    const before = await users.byName(this.source.manager, input.username);
    const matches = await this.passwords.verify(input.password, before?.password_hash);
    if (!matches || !before?.enabled) throw new IdentityError(401, 'AUTH_FAILED');
    return this.write(
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
  }

  /** 中心身份与访问地址无关；公开发现与已认证身份使用同一持久事实。 */
  async centerId(): Promise<string> {
    const center = await this.source.getRepository(CenterIdentityEntity).findOneBy({ singleton: true });
    if (!center) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return center.center_id;
  }

  async me(token?: string): Promise<IdentityResponse> {
    const center_id = await this.centerId();
    const row = await this.userForToken(this.source.manager, token);
    return { center_id, user: publicUser(row) };
  }

  /** 为后续代理保留已有的用途隔离；LLM key 只关联本次 Token，没有第二套生命周期。 */
  async llmIdentity(key: string): Promise<IdentityResponse> {
    return this.me(this.llmKeys.get(key));
  }

  discardLogin(token: string): void { this.revoke(token); }

  private revoke(token: string): void {
    const scope = this.tokens.get(token);
    this.tokens.delete(token);
    if (scope) for (const notify of this.sessionRevoked) notify(scope);
    for (const [key, owner] of this.llmKeys) if (owner === token) this.llmKeys.delete(key);
  }

  async logout(token: string | undefined, requestId: string): Promise<void> {
    await this.write(
      async (client) => {
        const id = token && this.tokens.get(token)?.userId;
        if (id) await this.audit(client, 'logout', requestId, id);
      },
      () => {
        if (token) this.revoke(token);
      },
    );
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
