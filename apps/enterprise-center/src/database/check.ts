import type { Pool } from 'pg';
import { checkDatabaseVersion } from './admission.js';
import { counts } from './counts.js';

/** 只读检查版本；关闭自动升级时调用本入口，不能隐式建表或更新账本。 */
export async function checkDatabase(pool: Pool, includeCounts = false) {
  const client = await pool.connect();
  try {
    await client.query('BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY');
    const identity = (await client.query<{ role: string; database: string; read_only: string }>(
      "SELECT current_user AS role,current_database() AS database,current_setting('transaction_read_only') AS read_only")).rows[0]!;
    const version = await checkDatabaseVersion(client);
    const rowCounts = includeCounts ? await counts(client) : undefined;
    await client.query('COMMIT');
    return { ...identity, schema_version: version, counts: rowCounts };
  } catch (error) {
    await client.query('ROLLBACK').catch(() => undefined);
    throw error;
  } finally { client.release(); }
}
