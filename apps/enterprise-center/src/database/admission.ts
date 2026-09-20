import type { PoolClient } from 'pg';
import { CenterError } from '../errors.js';
import { upgradeFiles } from './migrations.js';

function rejectVersion(): never { throw new CenterError('SCHEMA_MISMATCH', '数据库升级记录不兼容，请使用匹配的软件并核查升级记录。'); }

export async function checkServer(client: PoolClient): Promise<void> {
  const result = await client.query<{ version: string }>("SELECT current_setting('server_version_num') AS version");
  const version = Number(result.rows[0]!.version);
  if (!Number.isInteger(version) || version < 160014 || version >= 170000) throw new CenterError('DATABASE_VERSION_UNSUPPORTED', '当前中心支持 PostgreSQL 16.14 及以上的 16.x 版本。');
}

/** 只校验框架版本记录，未知/更高版本不能被旧程序忽略，不检查业务字段。 */
export async function readDatabaseVersion(client: PoolClient, names: readonly string[]): Promise<number> {
  const ledger = await client.query<{ ledger: string | null }>("SELECT to_regclass('public.pgmigrations')::text AS ledger");
  if (!ledger.rows[0]!.ledger) {
    if (await isUninitializedDatabase(client)) return 0;
    rejectVersion();
  }
  const rows = (await client.query<{ name: string }>('SELECT name FROM public.pgmigrations ORDER BY run_on,id')).rows;
  if (rows.length > names.length || rows.some((row, index) => row.name !== names[index])) rejectVersion();
  // 框架在首条 SQL 失败后可能留下空账本；只有尚无业务对象才允许再次初始化。
  if (!rows.length && !await isUninitializedDatabase(client)) rejectVersion();
  return rows.length;
}

export async function checkDatabaseVersion(client: PoolClient): Promise<number> {
  await checkServer(client);
  const { names } = upgradeFiles();
  const current = await readDatabaseVersion(client, names);
  if (current !== names.length) throw new CenterError('UPGRADE_REQUIRED', '数据库需要升级，请开启自动升级或执行 db:upgrade。');
  return current;
}

/** 只用于首次安装保护；框架自己的空账本、索引和序列不属于业务对象。 */
export async function isUninitializedDatabase(client: PoolClient): Promise<boolean> {
  const result = await client.query<{ empty: boolean }>(`SELECT NOT EXISTS (
    SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
      WHERE n.nspname='public' AND c.relkind NOT IN ('i','I') AND c.relname NOT IN ('pgmigrations','pgmigrations_id_seq')
    UNION ALL SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public'
    UNION ALL SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid=t.typnamespace WHERE n.nspname='public' AND t.typtype IN ('d','e','r','m')
    UNION ALL SELECT 1 FROM pg_namespace WHERE nspname NOT IN ('public','information_schema') AND nspname NOT LIKE 'pg_%'
    UNION ALL SELECT 1 FROM pg_extension WHERE extname <> 'plpgsql'
  ) AS empty`);
  return result.rows[0]!.empty;
}
