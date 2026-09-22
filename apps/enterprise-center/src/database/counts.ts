import type { PoolClient } from 'pg';

export const TABLES = [
  'center_identity',
  'management_audit',
  'pgmigrations',
  'users',
  'providers',
  'model_fixed_configs',
  'model_settings',
  'llm_recording_settings',
  'llm_calls',
] as const;

/** 只用于显式维护命令/验收的行数报告；普通启动不扫描业务数据。 */
export async function counts(client: PoolClient): Promise<Record<string, string>> {
  const result: Record<string, string> = {};
  for (const table of TABLES)
    result[table] = (await client.query<{ count: string }>(`SELECT count(*) FROM public.${table}`)).rows[0]!.count;
  return result;
}
