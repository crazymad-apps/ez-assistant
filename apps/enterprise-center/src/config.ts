import { isAbsolute, resolve } from 'node:path';
import { isIP } from 'node:net';
import { CenterError } from './errors.js';
import type { WebsocketLimits } from './llm/websocket.js';

export type DatabaseConfig = Readonly<{ url: string }>;
export type CenterConfig = Readonly<{
  database: DatabaseConfig;
  autoUpgrade: boolean;
  host: string;
  port: number;
  origin: string;
  templatesFile?: string;
  snapshotRoot?: string;
  pluginRoot?: string;
  siteContentRoot?: string;
  desktopReleaseRoot?: string;
  desktopUpdateKeyFile?: string;
  llmConnectTimeout?: number;
  llmIdleTimeout?: number;
  llmWebsocket?: WebsocketLimits;
}>;

function invalid(): never {
  throw new CenterError('INVALID_CONFIG', '中心配置无效；请检查数据库、监听地址与 public origin。');
}

export function databaseConfig(env: NodeJS.ProcessEnv): DatabaseConfig {
  const url = env.CENTER_DATABASE_URL;
  if (!url) return invalid();
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return invalid();
  }
  if (
    !['postgres:', 'postgresql:'].includes(parsed.protocol) ||
    !parsed.hostname ||
    !parsed.username ||
    parsed.pathname.length < 2
  )
    return invalid();
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
  try {
    origin = new URL(env.CENTER_PUBLIC_ORIGIN ?? '');
  } catch {
    return invalid();
  }
  if (origin.origin !== env.CENTER_PUBLIC_ORIGIN || origin.username || origin.password) return invalid();
  if (!['http:', 'https:'].includes(origin.protocol)) return invalid();

  const integer = (name: string, fallback: number, min: number, max: number) => {
    const raw = env[name];
    if (raw === undefined) return fallback;
    if (!/^[0-9]+$/.test(raw)) return invalid();
    const value = Number(raw);
    if (!Number.isSafeInteger(value) || value < min || value > max) return invalid();
    return value;
  };

  if (env.CENTER_SNAPSHOT_ROOT !== undefined && !isAbsolute(env.CENTER_SNAPSHOT_ROOT)) invalid();
  if (env.CENTER_PLUGIN_ROOT !== undefined && !isAbsolute(env.CENTER_PLUGIN_ROOT)) invalid();
  if (env.CENTER_DESKTOP_RELEASE_ROOT !== undefined && !isAbsolute(env.CENTER_DESKTOP_RELEASE_ROOT)) invalid();
  if (env.CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE !== undefined && !isAbsolute(env.CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE)) invalid();
  if (env.CENTER_SITE_CONTENT_ROOT !== undefined && !isAbsolute(env.CENTER_SITE_CONTENT_ROOT)) invalid();
  const llmWebsocket = {
    idleMs: integer('CENTER_LLM_WS_IDLE_MS', 600000, 1000, 86400000),
    maxConnections: integer('CENTER_LLM_WS_MAX_CONNECTIONS', 256, 1, 4096),
    maxPerUser: integer('CENTER_LLM_WS_MAX_PER_USER', 32, 1, 4096),
    maxBufferBytes: integer('CENTER_LLM_WS_MAX_BUFFER_BYTES', 16 * 1024 * 1024, 1024, 64 * 1024 * 1024),
  };
  if (llmWebsocket.maxPerUser > llmWebsocket.maxConnections) invalid();
  return {
    database,
    autoUpgrade: autoUpgrade === 'true',
    host,
    port,
    origin: origin.origin,
    templatesFile: env.CENTER_MODEL_TEMPLATES_FILE,
    siteContentRoot: env.CENTER_SITE_CONTENT_ROOT,
    snapshotRoot: env.CENTER_SNAPSHOT_ROOT,
    desktopReleaseRoot: env.CENTER_DESKTOP_RELEASE_ROOT ?? resolve(process.cwd(), 'data/desktop-releases'),
    desktopUpdateKeyFile: env.CENTER_DESKTOP_UPDATE_PUBLIC_KEY_FILE,
    pluginRoot: env.CENTER_PLUGIN_ROOT ?? resolve(process.cwd(), 'data/plugins'),
    llmConnectTimeout: integer('CENTER_LLM_CONNECT_TIMEOUT_MS', 10000, 1000, 120000),
    llmIdleTimeout: integer('CENTER_LLM_IDLE_TIMEOUT_MS', 300000, 1000, 3600000),
    llmWebsocket,
  };
}

export function databaseTarget(config: DatabaseConfig) {
  const uri = new URL(config.url);
  return {
    host: uri.hostname,
    port: Number(uri.port || 5432),
    database: decodeURIComponent(uri.pathname.slice(1)),
    role: decodeURIComponent(uri.username),
  };
}
