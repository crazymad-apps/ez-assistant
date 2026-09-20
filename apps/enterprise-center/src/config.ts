import { isIP } from 'node:net';
import { CenterError } from './errors.js';

export type DatabaseConfig = Readonly<{ url: string }>;
export type CenterConfig = Readonly<{ database: DatabaseConfig; autoUpgrade: boolean; host: string; port: number; origin: string }>;

function invalid(): never { throw new CenterError('INVALID_CONFIG', '中心配置无效；请检查数据库、监听地址与 public origin。'); }

export function databaseConfig(env: NodeJS.ProcessEnv): DatabaseConfig {
  const url = env.CENTER_DATABASE_URL;
  if (!url) return invalid();
  let parsed: URL;
  try { parsed = new URL(url); } catch { return invalid(); }
  if (!['postgres:', 'postgresql:'].includes(parsed.protocol) || !parsed.hostname || !parsed.username || parsed.pathname.length < 2) return invalid();
  // 不允许 libpq URI 查询参数重定向到另一个主机/库，避免核对目标与真实连接不一致。
  for (const key of parsed.searchParams.keys()) if (key !== 'sslmode') return invalid();
  return { url };
}

export function loadConfig(env: NodeJS.ProcessEnv): CenterConfig {
  const database = databaseConfig(env);
  const autoUpgrade = env.CENTER_DATABASE_AUTO_UPGRADE ?? 'true';
  if (!['true', 'false'].includes(autoUpgrade)) return invalid();
  const host = env.CENTER_HOST ?? '127.0.0.1';
  const portText = env.CENTER_PORT ?? '7320';
  if (!isIP(host) || !/^\d+$/.test(portText)) return invalid();
  const port = Number(portText);
  if (port < 1 || port > 65535) return invalid();
  let origin: URL;
  try { origin = new URL(env.CENTER_PUBLIC_ORIGIN ?? ''); } catch { return invalid(); }
  if (origin.origin !== env.CENTER_PUBLIC_ORIGIN || origin.username || origin.password) return invalid();
  const loopback = ['127.0.0.1', '::1'].includes(host);
  const localOrigin = ['127.0.0.1', '[::1]', 'localhost'].includes(origin.hostname);
  if (origin.protocol !== 'https:' && !(origin.protocol === 'http:' && loopback && localOrigin && env.CENTER_ALLOW_HTTP_LOOPBACK === 'true')) return invalid();
  return { database, autoUpgrade: autoUpgrade === 'true', host, port, origin: origin.origin };
}

export function databaseTarget(config: DatabaseConfig) {
  const uri = new URL(config.url);
  return { host: uri.hostname, port: Number(uri.port || 5432), database: decodeURIComponent(uri.pathname.slice(1)), role: decodeURIComponent(uri.username) };
}
