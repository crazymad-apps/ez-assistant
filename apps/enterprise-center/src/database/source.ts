import 'reflect-metadata';
import { DataSource } from 'typeorm';
import type { DatabaseConfig } from '../config.js';
import { UserEntity } from '../users/entity.js';
import { CenterIdentityEntity } from '../identity/entity.js';
import { AuditEntity } from '../audit/entity.js';

/** 唯一业务连接池；仅在 SQL 升级/准入完成后初始化，不运行任何 Schema 同步。 */
export function createDataSource(config: DatabaseConfig): DataSource {
  return new DataSource({ type: 'postgres', url: config.url, schema: 'public',
    entities: [UserEntity, CenterIdentityEntity, AuditEntity],
    synchronize: false, dropSchema: false, migrationsRun: false, migrations: [],
    logging: false, installExtensions: false, poolSize: 8, connectTimeoutMS: 3000,
    // ORM 的驱动诊断也不可输出原始 SQL、参数或连接秘密。
    logger: { log() {}, logQuery() {}, logQueryError() {}, logQuerySlow() {}, logSchemaBuild() {}, logMigration() {} },
    poolErrorHandler: () => { process.stderr.write('CENTER_DATABASE_CONNECTION_LOST\n'); },
    extra: { idleTimeoutMillis: 10000, statement_timeout: 5000, lock_timeout: 2000,
      idle_in_transaction_session_timeout: 10000, application_name: 'ez-enterprise-center',
      options: '-c search_path=pg_catalog,public' },
  });
}
