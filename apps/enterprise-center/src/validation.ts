import { IdentityError } from './identity/errors.js';

export function fields(value: unknown, names: readonly string[], optional: readonly string[] = []): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).some(name => !names.includes(name) && !optional.includes(name))
    || names.some(name => !Object.hasOwn(value, name))) {
    throw new IdentityError(400, 'INVALID_REQUEST');
  }
  return value as Record<string, unknown>;
}
export function invalid(): never { throw new IdentityError(400, 'INVALID_REQUEST'); }
/** URL 中的数据库 ID 只接受规范十进制正整数，与 PostgreSQL integer 范围一致。 */
export function recordId(value: unknown): number {
  if (typeof value !== 'string' || !/^[1-9][0-9]{0,9}$/.test(value)) invalid();
  const id = Number(value);
  if (id > 2147483647) invalid();
  return id;
}
/** 查询参数只接受单值字符串，拒绝重复键、数组和未知字段；不依赖 TS DTO 自动转换。 */
export function queryFields(value: unknown, names: readonly string[]): Record<string, string> {
  const input = fields(value, [], names);
  for (const v of Object.values(input)) if (typeof v !== 'string' || v.includes('\0')) invalid();
  return input as Record<string, string>;
}
export function page(input: Record<string, string>): { limit: number; offset: number } {
  const parse = (key: string, fallback: number, min: number, max: number) => {
    const raw = input[key]; if (raw === undefined) return fallback;
    if (!/^(0|[1-9][0-9]{0,6})$/.test(raw)) invalid();
    const number = Number(raw); if (number < min || number > max) invalid(); return number;
  };
  return { limit: parse('limit', 20, 1, 100), offset: parse('offset', 0, 0, 1000000) };
}
export function queryBoolean(value: string): boolean { if (value !== 'true' && value !== 'false') invalid(); return value === 'true'; }
