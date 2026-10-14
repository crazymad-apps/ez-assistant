import { IdentityError } from '../identity/errors.js';

export const limits = { total: 40 * 1024 ** 2, entries: 256, text: 2 * 1024 ** 2, document: 128 * 1024, image: 5 * 1024 ** 2, projection: 4 * 1024 ** 2 };
export function invalid(): never { throw new IdentityError(400, 'SITE_CONTENT_INVALID'); }
export function record(value: unknown, keys: string[]): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).some(key => !keys.includes(key))) invalid();
  return value as Record<string, unknown>;
}
export function text(value: unknown, max = 4096): string {
  if (typeof value !== 'string' || !value.trim() || value.includes('\0') || Buffer.byteLength(value) > max) invalid();
  return value;
}
export function list(value: unknown): unknown[] { if (!Array.isArray(value) || value.length > 256) invalid(); return value; }
export function slug(value: unknown): string { const result = text(value, 100); if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(result)) invalid(); return result; }
export function path(value: unknown): string {
  const result = text(value, 200);
  if (!/^[a-zA-Z0-9_-]+(?:\/[a-zA-Z0-9_-]+)*(?:\.[a-zA-Z0-9]+)+$/.test(result)) invalid();
  return result;
}
export function date(value: unknown): string {
  const result = text(value, 10);
  if (!/^\d{4}-\d{2}-\d{2}$/.test(result) || !Number.isFinite(Date.parse(result)) || new Date(result).toISOString().slice(0, 10) !== result) invalid();
  return result;
}
export function utf8(bytes: Buffer): string { try { return new TextDecoder('utf-8', { fatal: true }).decode(bytes); } catch { return invalid(); } }

/** 本地内容工具只报告已校验相对文件及固定原因，不泄露底层异常或绝对路径。 */
export class ContentSourceError extends Error {
  constructor(file: string, reason: string) { super(`${file}：${reason}`); }
}
export function inSource<T>(file: string, reason: string, work: () => T): T {
  try { return work(); } catch (error) {
    if (error instanceof ContentSourceError) throw error;
    throw new ContentSourceError(file, reason);
  }
}
