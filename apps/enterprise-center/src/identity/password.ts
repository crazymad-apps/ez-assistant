import { randomBytes, scrypt, timingSafeEqual } from 'node:crypto';
import { IdentityError } from './errors.js';

// 固定假摘要只用于不存在账号的等成本计算，不作为任何账号的密码。
const dummy = 'scrypt$131072$8$1$00000000000000000000000000000000$' + '00'.repeat(32);

/** 每个应用实例最多两个 scrypt 和八个等待者；不在数据库事务/锁内消耗密码计算资源。 */
export class Passwords {
  private active = 0;
  private readonly waiting: Array<() => void> = [];

  private async derive(password: string, salt: Buffer): Promise<Buffer> {
    if (this.active >= 2) {
      if (this.waiting.length >= 8) throw new IdentityError(429, 'RATE_LIMITED');
      await new Promise<void>(resolve => this.waiting.push(resolve));
    } else this.active++;
    try {
      return await new Promise<Buffer>((resolve, reject) => scrypt(password, salt, 32,
        { N: 131072, r: 8, p: 1, maxmem: 192 * 1024 * 1024 }, (error, key) => error ? reject(error) : resolve(key)));
    } finally {
      // 直接把计算槽交给队首，不能在唤醒与下一次进入之间超出并发上限。
      const next = this.waiting.shift();
      if (next) next(); else this.active--;
    }
  }

  async verify(password: string, encoded?: string): Promise<boolean> {
    const hash = encoded ?? dummy;
    const match = /^scrypt\$131072\$8\$1\$([a-f0-9]{32})\$([a-f0-9]{64})$/.exec(hash);
    if (!match) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const derived = await this.derive(password, Buffer.from(match[1]!, 'hex'));
    return timingSafeEqual(derived, Buffer.from(match[2]!, 'hex')) && encoded !== undefined;
  }

  async hash(password: string): Promise<string> {
    const salt = randomBytes(16);
    const key = await this.derive(password, salt);
    return `scrypt$131072$8$1$${salt.toString('hex')}$${key.toString('hex')}`;
  }
}
