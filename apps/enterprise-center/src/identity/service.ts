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

/** 单实例进程拥有 Token；不持久化、不计算摘要、不设 TTL，关闭或重启全部失效。 */
export class IdentityService {
  readonly passwords = new Passwords();
  readonly limiter = new LoginLimiter();
  private readonly tokens = new Map<string, number>();
  private readonly llmKeys = new Map<string, string>();
  private pendingWrite: Promise<void> = Promise.resolve();

  constructor(private readonly connection?: DataSource) {}

  private get source(): DataSource {
    if (!this.connection?.isInitialized) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    return this.connection;
  }

  onModuleDestroy(): void {
    this.tokens.clear();
    this.llmKeys.clear();
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

  /** 仅在身份事务成功提交后调用；角色和启停变化撤销；改密与重置保留已有凭据。 */
  revokeUser(id: number): void {
    for (const [token, userId] of this.tokens) if (userId === id) this.revoke(token);
  }

  private async userForToken(client: EntityManager, token?: string) {
    const id = token && this.tokens.get(token);
    if (!id) throw new IdentityError(401, 'TOKEN_INVALID');
    const row = await users.forIdentity(client, id);
    // 查询期间可能已经退出；不把异步查询前的认证结果当成仍然有效。
    if (!row?.enabled || this.tokens.get(token!) !== id) throw new IdentityError(401, 'TOKEN_INVALID');
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
        const center = await client.getRepository(CenterIdentityEntity).findOneBy({ singleton: true });
        if (!center) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
        const token = `ct_${randomBytes(32).toString('hex')}`,
          llm_key = `cl_${randomBytes(32).toString('hex')}`;
        await this.audit(client, 'login', requestId, user.id);
        return { center_id: center.center_id, user: publicUser(user), token, llm_key };
      },
      (result) => {
        this.tokens.set(result.token, result.user.id);
        this.llmKeys.set(result.llm_key, result.token);
      },
    );
  }

  async me(token?: string): Promise<IdentityResponse> {
    const center = await this.source.getRepository(CenterIdentityEntity).findOneBy({ singleton: true });
    if (!center) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const row = await this.userForToken(this.source.manager, token);
    return { center_id: center.center_id, user: publicUser(row) };
  }

  /** 为后续代理保留已有的用途隔离；LLM key 只关联本次 Token，没有第二套生命周期。 */
  async llmIdentity(key: string): Promise<IdentityResponse> {
    return this.me(this.llmKeys.get(key));
  }

  private revoke(token: string): void {
    this.tokens.delete(token);
    for (const [key, owner] of this.llmKeys) if (owner === token) this.llmKeys.delete(key);
  }

  async logout(token: string | undefined, requestId: string): Promise<void> {
    await this.write(
      async (client) => {
        const id = token && this.tokens.get(token);
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
