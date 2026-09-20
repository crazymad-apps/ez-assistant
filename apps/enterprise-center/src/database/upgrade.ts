import type { Pool } from 'pg';
import { runner } from 'node-pg-migrate';
import { checkDatabaseVersion, checkServer, readDatabaseVersion } from './admission.js';
import { upgradeFiles } from './migrations.js';

/** 升级和业务共用账号。框架拥有锁/事务/账本；这里只保留目标准入及固定脱敏日志边界。 */
export async function upgradeDatabase(pool: Pool) {
  const client = await pool.connect();
  try {
    await checkServer(client);
    const { directory, names } = upgradeFiles();
    await readDatabaseVersion(client, names);
    const applied = await runner({
      dbClient: client, dir: directory, direction: 'up', migrationsTable: 'pgmigrations',
      schema: 'public', migrationsSchema: 'public', createSchema: false, createMigrationsSchema: false,
      checkOrder: true, singleTransaction: true, noLock: false, advisoryLockMode: 'fail', verbose: false,
      // 框架默认会输出 SQL 和原始异常；只允许上层输出 safeError，不透传框架日志。
      logger: { info() {}, warn() {}, error() {} },
    });
    await checkDatabaseVersion(client);
    return { applied: applied.map(item => item.name) };
  } finally {
    // 框架使用会话级锁并修改 search_path；维护池也不复用这条带会话状态的连接。
    client.release(true);
  }
}
