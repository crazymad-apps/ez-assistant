import type { EntityManager } from 'typeorm';
import { invalid, queryFields, recordId } from '../validation.js';
import { enumeration, text, uuid } from '../models/parameters.js';
import { outcomes } from '../contracts/calls.js';
import type { UsageBucket, UsageGroup, UsageModelOption, UsagePage, UsageTotals } from '../contracts/calls.js';

const zone = 'Asia/Shanghai';
const metrics = ['input_tokens', 'output_tokens', 'total_tokens', 'cached_input_tokens', 'reasoning_tokens'] as const;

function aggregates(): string {
  return [
    'COUNT(*)::text AS calls',
    ...outcomes.map((outcome) => `COUNT(*) FILTER (WHERE outcome = '${outcome}')::text AS ${outcome}`),
    'ROUND(AVG(EXTRACT(EPOCH FROM (ended_at - started_at)) * 1000))::text AS avg_duration_ms',
    ...(['available', 'partial', 'unavailable', 'pending'] as const).map(
      (state) => `COUNT(*) FILTER (WHERE usage_state = '${state}')::text AS ${state}`,
    ),
    ...metrics.flatMap((name) => [
      `SUM(${name})::text AS ${name}`,
      `COUNT(${name})::text AS ${name.replace('_tokens', '')}_samples`,
    ]),
  ].join(', ');
}

type RawTotals = Partial<Record<keyof UsageTotals | 'cached_input_samples', string | null>>;

function totals(row: RawTotals | undefined): UsageTotals {
  const calls = row?.calls ?? '0';
  const metric = (name: (typeof metrics)[number]) => row?.[name] ?? (calls === '0' ? '0' : null);
  return {
    calls,
    forwarded: row?.forwarded ?? '0',
    rejected: row?.rejected ?? '0',
    upstream_error: row?.upstream_error ?? '0',
    interrupted: row?.interrupted ?? '0',
    unknown: row?.unknown ?? '0',
    in_progress: row?.in_progress ?? '0',
    avg_duration_ms: row?.avg_duration_ms ?? null,
    available: row?.available ?? '0',
    partial: row?.partial ?? '0',
    unavailable: row?.unavailable ?? '0',
    pending: row?.pending ?? '0',
    input_tokens: metric('input_tokens'),
    output_tokens: metric('output_tokens'),
    total_tokens: metric('total_tokens'),
    cached_input_tokens: metric('cached_input_tokens'),
    reasoning_tokens: metric('reasoning_tokens'),
    input_samples: row?.input_samples ?? '0',
    output_samples: row?.output_samples ?? '0',
    total_samples: row?.total_samples ?? '0',
    cached_samples: row?.cached_input_samples ?? '0',
    reasoning_samples: row?.reasoning_samples ?? '0',
  };
}

function timestamp(value: string | undefined, fallback: Date): Date {
  if (value === undefined) return fallback;
  if (!/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d{1,3})?Z$/.test(value)) invalid();
  const date = new Date(value);
  if (!Number.isFinite(date.getTime()) || date.toISOString().slice(0, 19) !== value.slice(0, 19)) invalid();
  return date;
}

function integer(value: string | undefined, fallback: number, min: number, max: number): number {
  if (value === undefined) return fallback;
  if (!/^(0|[1-9][0-9]*)$/.test(value)) invalid();
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) invalid();
  return parsed;
}

function bucketStart(date: Date, granularity: 'hour' | 'day' | 'month'): Date {
  const shifted = new Date(date.getTime() + 8 * 3600000);
  if (granularity === 'month') shifted.setUTCDate(1);
  if (granularity !== 'hour') shifted.setUTCHours(0);
  shifted.setUTCMinutes(0, 0, 0);
  return new Date(shifted.getTime() - 8 * 3600000);
}

function nextBucket(date: Date, granularity: 'hour' | 'day' | 'month'): Date {
  if (granularity === 'hour') return new Date(date.getTime() + 3600000);
  if (granularity === 'day') return new Date(date.getTime() + 86400000);
  const local = new Date(date.getTime() + 8 * 3600000);
  local.setUTCMonth(local.getUTCMonth() + 1);
  return new Date(local.getTime() - 8 * 3600000);
}

export async function queryUsage(manager: EntityManager, value: unknown): Promise<UsagePage> {
  const input = queryFields(value, [
    'from', 'to', 'user_id', 'provider_instance_id', 'model_id', 'kind', 'outcome', 'granularity', 'group_by', 'limit', 'offset',
  ]);
  const now = new Date();
  const from = timestamp(input.from, new Date(now.getTime() - 7 * 86400000));
  const to = timestamp(input.to, now);
  const granularity = enumeration(input.granularity ?? 'day', ['hour', 'day', 'month']);
  const groupBy = enumeration(input.group_by ?? 'model', ['user', 'model']);
  const limit = integer(input.limit, 20, 1, 100);
  const offset = integer(input.offset, 0, 0, 1000000);
  if (from >= to || (input.provider_instance_id === undefined) !== (input.model_id === undefined)) invalid();
  const starts: string[] = [];
  for (let bucket = bucketStart(from, granularity); bucket < to; bucket = nextBucket(bucket, granularity)) {
    if (starts.length === 1000) invalid();
    starts.push(bucket.toISOString());
  }
  const conditions = ['started_at >= $1', 'started_at < $2'];
  const params: unknown[] = [from, to];
  const add = (sql: string, argument: unknown) => {
    params.push(argument);
    conditions.push(`${sql} $${params.length}`);
  };
  if (input.user_id !== undefined) add('user_id =', recordId(input.user_id));
  if (input.provider_instance_id !== undefined) {
    add('provider_instance_id =', uuid(input.provider_instance_id));
    add('model_id =', text(input.model_id, 1024));
  }
  if (input.kind === undefined) conditions.push("kind IN ('proxy', 'context_compaction')");
  else add('kind =', enumeration(input.kind, ['proxy', 'admin_test', 'context_compaction']));
  if (input.outcome !== undefined) add('outcome =', enumeration(input.outcome, outcomes));
  const filtered = `FROM public.llm_calls WHERE ${conditions.join(' AND ')}`;
  const expression = `date_trunc('${granularity}', started_at AT TIME ZONE '${zone}') AT TIME ZONE '${zone}'`;
  const groupKey = groupBy === 'user' ? 'user_id' : 'provider_instance_id, model_id';
  const selectKey = groupBy === 'user' ? 'user_id' : 'provider_instance_id, model_id';
  const [summaryRows, trendRows, groupRows, countRows, settingsRows] = await manager.transaction('REPEATABLE READ', async (tx) => {
    // 一个 QueryRunner 顺序读取，保持同一快照且不并发使用 pg client。
    const summary = await tx.query(`SELECT ${aggregates()} ${filtered}`, params) as RawTotals[];
    const trend = await tx.query(`SELECT ${expression} AS start, ${aggregates()} ${filtered} GROUP BY start ORDER BY start`, params) as (RawTotals & { start: Date })[];
    const groups = await tx.query(`SELECT ${selectKey}, MAX(username) AS username, MAX(provider_name) AS provider_name, ${aggregates()} ${filtered} GROUP BY ${groupKey} ORDER BY SUM(total_tokens) DESC NULLS LAST, ${groupKey} LIMIT ${limit} OFFSET ${offset}`, params) as (RawTotals & { user_id: number | null; username: string | null; provider_instance_id: string | null; provider_name: string | null; model_id: string | null })[];
    const counts = await tx.query(`SELECT COUNT(*)::integer AS total FROM (SELECT 1 ${filtered} GROUP BY ${groupKey}) g`, params) as { total: number }[];
    const settings = await tx.query('SELECT index_retention_days, usage_available_from FROM public.llm_recording_settings WHERE singleton = true') as { index_retention_days: number; usage_available_from: Date }[];
    return [summary, trend, groups, counts, settings] as const;
  });
  const settings = settingsRows[0];
  if (!settings) throw new Error('MISSING_LLM_RECORDING_SETTINGS');
  const retention = new Date(now.getTime() - settings.index_retention_days * 86400000);
  const availableFrom = new Date(Math.max(retention.getTime(), settings.usage_available_from.getTime()));
  const mapped = new Map(trendRows.map((row) => [new Date(row.start).toISOString(), row]));
  const buckets: UsageBucket[] = starts.map((start) => {
    const end = nextBucket(new Date(start), granularity);
    return { start, ...totals(mapped.get(start)), incomplete: new Date(start) < availableFrom || end > now || end > to };
  });
  const groups: UsageGroup[] = groupRows.map((row) => ({
    ...totals(row),
    user_id: groupBy === 'user' ? row.user_id : null,
    username: groupBy === 'user' ? row.username : null,
    provider_instance_id: groupBy === 'model' ? row.provider_instance_id : null,
    provider_name: groupBy === 'model' ? row.provider_name : null,
    model_id: groupBy === 'model' ? row.model_id : null,
  }));
  return {
    from: from.toISOString(), to: to.toISOString(), available_from: availableFrom.toISOString(), timezone: zone,
    granularity, group_by: groupBy, summary: totals(summaryRows[0]), buckets, groups,
    group_total: countRows[0]?.total ?? 0, limit, offset,
  };
}

export async function usageModels(manager: EntityManager, value: unknown): Promise<UsageModelOption[]> {
  const input = queryFields(value, ['search']);
  const search = input.search === undefined ? '' : text(input.search, 128, true);
  const escaped = search.replace(/[\\%_]/g, '\\$&');
  return manager.query(
    `SELECT DISTINCT ON (provider_instance_id, model_id) provider_instance_id, model_id,
      COALESCE(provider_name, '未知服务商') AS provider_name
     FROM public.llm_calls
     WHERE provider_instance_id IS NOT NULL AND model_id IS NOT NULL
       AND (provider_name ILIKE $1 ESCAPE '\\' OR model_id ILIKE $1 ESCAPE '\\')
     ORDER BY provider_instance_id, model_id, started_at DESC LIMIT 100`,
    [`%${escaped}%`],
  ) as Promise<UsageModelOption[]>;
}
