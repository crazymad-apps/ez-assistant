import type { DataSource } from 'typeorm';
import type { AuditDetails, AuditPage, AuditQuery, AuditUserFields } from '../contracts/audit.js';
import { audits } from './repository.js';
import { IdentityError } from '../identity/errors.js';
import { invalid, page, queryBoolean, queryFields, recordId } from '../validation.js';

export function auditQuery(value: unknown): AuditQuery & { limit: number; offset: number } {
  const input = queryFields(value, ['id', 'from', 'to', 'actor_user_id', 'action', 'success', 'limit', 'offset']);
  for (const key of ['from', 'to']) {
    const text = input[key]; if (text === undefined) continue;
    if (!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z$/.test(text) || !Number.isFinite(Date.parse(text))
      || new Date(text).toISOString().slice(0, 19) !== text.slice(0, 19)) invalid();
  }
  if (input.from && input.to && Date.parse(input.from) >= Date.parse(input.to)) invalid();
  if (input.action !== undefined && !/^[a-z_]{1,64}$/.test(input.action)) invalid();
  return { ...page(input), ...(input.from === undefined ? {} : { from: input.from }), ...(input.to === undefined ? {} : { to: input.to }),
    ...(input.id === undefined ? {} : { id: recordId(input.id) }),
    ...(input.actor_user_id === undefined ? {} : { actor_user_id: recordId(input.actor_user_id) }),
    ...(input.action === undefined ? {} : { action: input.action }),
    ...(input.success === undefined ? {} : { success: queryBoolean(input.success) }) };
}

/** 查询时再次投影允许字段，不能把任意 JSON 详情（含人工 SQL 写入内容）直接返回。 */
export function auditDetails(value: unknown): AuditDetails {
  if (!value || typeof value !== 'object') return {};
  const result: { before?: AuditUserFields; after?: AuditUserFields } = {};
  for (const key of ['before', 'after'] as const) {
    const source: unknown = Reflect.get(value, key);
    if (!source || typeof source !== 'object') continue;
    const name: unknown = Reflect.get(source, 'display_name'), role: unknown = Reflect.get(source, 'role'), enabled: unknown = Reflect.get(source, 'enabled');
    result[key] = { ...(typeof name === 'string' && [...name].length <= 64 ? { display_name: name } : {}),
      ...(role === 'user' || role === 'admin' ? { role } : {}), ...(typeof enabled === 'boolean' ? { enabled } : {}) };
  }
  return result;
}

export class AuditService {
  constructor(private readonly source?: DataSource) {}
  async list(input: ReturnType<typeof auditQuery>): Promise<AuditPage> {
    if (!this.source) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    const row = await audits.list(this.source.manager, input);
    return { items: row.items.map(item => ({ ...item, occurred_at: new Date(item.occurred_at).toISOString(),
      details: auditDetails(item.details) })), total: row.total, limit: input.limit, offset: input.offset };
  }
}
