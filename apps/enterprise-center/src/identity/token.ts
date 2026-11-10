import { randomBytes, randomUUID } from 'node:crypto';
import { SignJWT, jwtVerify } from 'jose';
import { fields, invalid, uuid } from '../validation.js';
import { IdentityError } from './errors.js';

export type Scope = { kind: 'unselected' } | { kind: 'system' } | { kind: 'organization'; organization_id: string };

export function parseScope(value: unknown): Scope {
  const input = fields(value, ['kind'], ['organization_id']);
  if (input.kind === 'organization') return { kind: 'organization', organization_id: uuid(input.organization_id) };
  if ((input.kind !== 'system' && input.kind !== 'unselected') || Object.hasOwn(input, 'organization_id')) invalid();
  return { kind: input.kind };
}

export function sameScope(left: Scope, right: Scope): boolean {
  if (left.kind === 'organization' && right.kind === 'organization')
    return left.organization_id === right.organization_id;
  return left.kind === right.kind;
}

const lifetime = 7 * 24 * 60 * 60;
const audience = 'ez-enterprise-center-api';
const algorithm = 'HS256';

export class CenterTokens {
  private readonly key = randomBytes(32);

  async issue(issuer: string, userId: number, scope: Scope) {
    const jti = randomUUID();
    const issuedAt = Math.floor(Date.now() / 1000);
    const expires = issuedAt + lifetime;
    const token = await new SignJWT({ scope })
      .setProtectedHeader({ alg: algorithm, typ: 'JWT' })
      .setIssuer(issuer)
      .setAudience(audience)
      .setSubject(String(userId))
      .setJti(jti)
      .setIssuedAt(issuedAt)
      .setExpirationTime(expires)
      .sign(this.key);
    return { token, jti, expires };
  }

  async verify(token: string | undefined) {
    try {
      if (!token || token.length > 2048) throw new Error('invalid token length');
      const { payload } = await jwtVerify(token, this.key, {
        algorithms: [algorithm], audience, typ: 'JWT', requiredClaims: ['sub', 'iss', 'iat', 'exp', 'jti', 'scope'],
      });
      const { exp, iat, sub, iss, jti, aud, nbf } = payload;
      if (
        typeof iat !== 'number' || !Number.isSafeInteger(iat) || iat < 0 ||
        iat > Math.floor(Date.now() / 1000) || exp !== iat + lifetime ||
        typeof sub !== 'string' || !/^[1-9][0-9]{0,9}$/.test(sub) || Number(sub) > 2147483647 ||
        aud !== audience || nbf !== undefined
      ) throw new Error('invalid token claims');
      return { jti: uuid(jti), issuer: uuid(iss), userId: Number(sub), expires: exp, scope: parseScope(payload.scope) };
    } catch {
      throw new IdentityError(401, 'TOKEN_INVALID');
    }
  }
}
