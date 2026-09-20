import { fields } from '../validation.js';
import { IdentityError } from './errors.js';
import type { LoginRequest, PasswordRequest } from '../contracts/identity.js';

export function password(value: unknown, fresh = false): string {
  // 登录/原密码只校验输入边界，历史密码不因新策略而无法登录。
  if (typeof value !== 'string' || [...value].length < (fresh ? 6 : 1)
    || [...value].length > 128 || Buffer.byteLength(value, 'utf8') > 512
    || (fresh && (!/[a-zA-Z]/.test(value) || !/[0-9]/.test(value)))) throw new IdentityError(400, 'INVALID_REQUEST');
  return value;
}
export function loginInput(value: unknown): LoginRequest {
  const input = fields(value, ['username', 'password']);
  return { username: username(input.username), password: password(input.password) };
}
export function username(value: unknown): string {
  if (typeof value !== 'string' || value.length > 128) throw new IdentityError(400, 'INVALID_REQUEST');
  const result = value.trim().toLowerCase();
  if (!/^[a-z0-9._-]{3,64}$/.test(result)) throw new IdentityError(400, 'INVALID_REQUEST');
  return result;
}
export function passwordInput(value: unknown): PasswordRequest {
  const input = fields(value, ['old_password', 'new_password']);
  return { old_password: password(input.old_password), new_password: password(input.new_password, true) };
}

/** 以真实 TCP 来源限流，不信任 X-Forwarded-For；不按任意账号创建永久状态。 */
export class LoginLimiter {
  private readonly sources = new Map<string, { start: number; count: number }>();
  take(ip: string, now = Date.now()): void {
    for (const [key, value] of this.sources) if (now - value.start >= 60000) this.sources.delete(key);
    let entry = this.sources.get(ip);
    if (!entry) {
      if (this.sources.size >= 1024) throw new IdentityError(429, 'RATE_LIMITED');
      entry = { start: now, count: 0 }; this.sources.set(ip, entry);
    }
    if (++entry.count > 20) throw new IdentityError(429, 'RATE_LIMITED');
  }
}
