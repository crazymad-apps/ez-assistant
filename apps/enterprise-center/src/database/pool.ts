import pg from 'pg';
import type { DatabaseConfig } from '../config.js';

export function createPool(config: DatabaseConfig): pg.Pool {
  const pool = new pg.Pool({ connectionString: config.url, max: 8, connectionTimeoutMillis: 3000,
    idleTimeoutMillis: 10000, statement_timeout: 5000, lock_timeout: 2000,
    idle_in_transaction_session_timeout: 10000, application_name: 'ez-enterprise-center',
    options: '-c search_path=pg_catalog,public' });
  // 空闲连接断开也必须消费 error；不输出可能包含连接串的 pg 原始异常。
  pool.on('error', () => { process.stderr.write('CENTER_DATABASE_CONNECTION_LOST\n'); });
  return pool;
}
