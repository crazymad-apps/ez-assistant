import { describe, expect, it } from 'vitest';
import { databaseConfig, databaseTarget, loadConfig } from '../../dist/config.js';
import { safeError } from '../../dist/errors.js';

const env = {
  CENTER_DATABASE_URL: 'postgres://center_app:secret@127.0.0.1:55432/center_test',
  CENTER_PUBLIC_ORIGIN: 'http://127.0.0.1:7320',
};

describe('显式配置与脱敏', () => {
  it('允许显式 loopback 开发配置', () => {
    expect(loadConfig(env).port).toBe(7320);
  });
  it('数据库无默认目标', () => {
    expect(() => databaseConfig({})).toThrow();
  });
  it('目标报告不包含密码', () => {
    expect(JSON.stringify(databaseTarget(databaseConfig(env)))).not.toContain('secret');
  });
  it.each(['?host=elsewhere', '?dbname=other', '?options=-csearch_path=evil'])('禁止 URI 重定向 %s', (suffix) => {
    expect(() => databaseConfig({ ...env, CENTER_DATABASE_URL: env.CENTER_DATABASE_URL + suffix })).toThrow();
  });
  it('单一数据库连接足够，不需要额外角色配置', () => {
    expect(databaseConfig(env)).toEqual({ url: env.CENTER_DATABASE_URL });
  });
  it('默认开启自动升级', () => {
    expect(loadConfig(env).autoUpgrade).toBe(true);
  });
  it.each(['true', 'false'])('显式自动升级开关 %s', (value) => {
    expect(loadConfig({ ...env, CENTER_DATABASE_AUTO_UPGRADE: value }).autoUpgrade).toBe(value === 'true');
  });
  it.each(['', 'yes', '1', 'FALSE'])('拒绝误拼的自动升级开关 %s', (value) => {
    expect(() => loadConfig({ ...env, CENTER_DATABASE_AUTO_UPGRADE: value })).toThrow();
  });
  it('默认允许 IP HTTP，仍保留精确 origin', () => {
    const config = loadConfig({ ...env, CENTER_HOST: '0.0.0.0', CENTER_PUBLIC_ORIGIN: 'http://192.0.2.10:7320' });
    expect(config.origin).toBe('http://192.0.2.10:7320');
  });
  it.each(['ftp://center.example', 'ws://center.example'])('拒绝非 HTTP/HTTPS 协议 %s', (origin) => {
    expect(() => loadConfig({ ...env, CENTER_PUBLIC_ORIGIN: origin })).toThrow();
  });
  it('HTTPS origin 允许非本地显式监听', () => {
    expect(loadConfig({ ...env, CENTER_HOST: '0.0.0.0', CENTER_PUBLIC_ORIGIN: 'https://center.example' }).host).toBe(
      '0.0.0.0',
    );
  });
  it.each(['0', '65536', '7320junk'])('拒绝非法端口 %s', (port) => {
    expect(() => loadConfig({ ...env, CENTER_PORT: port })).toThrow();
  });
  it('未知异常不透传内容', () => {
    expect(JSON.stringify(safeError(new Error('password=secret SQL SELECT')))).not.toMatch(/secret|SELECT/);
  });
});

describe('WebSocket 资源配置', () => {
  it('默认值与显式覆盖', () => {
    expect(loadConfig(env).llmWebsocket).toEqual({
      idleMs: 600000,
      maxConnections: 256,
      maxPerUser: 32,
      maxBufferBytes: 16777216,
    });
    expect(
      loadConfig({
        ...env,
        CENTER_LLM_WS_IDLE_MS: '1000',
        CENTER_LLM_WS_MAX_CONNECTIONS: '2',
        CENTER_LLM_WS_MAX_PER_USER: '1',
        CENTER_LLM_WS_MAX_BUFFER_BYTES: '1024',
      }).llmWebsocket,
    ).toEqual({ idleMs: 1000, maxConnections: 2, maxPerUser: 1, maxBufferBytes: 1024 });
  });
  it.each(['0', '-1', 'abc', '2.5', '99999999999999999'])('非法整数被拒绝 %s', (value) => {
    expect(() => loadConfig({ ...env, CENTER_LLM_WS_MAX_CONNECTIONS: value })).toThrow();
  });
  it('单用户上限不能超过总量，期限和缓冲都有边界', () => {
    expect(() => loadConfig({ ...env, CENTER_LLM_WS_MAX_CONNECTIONS: '1' })).toThrow();
    expect(() => loadConfig({ ...env, CENTER_LLM_WS_IDLE_MS: '86400001' })).toThrow();
    expect(() => loadConfig({ ...env, CENTER_LLM_WS_MAX_BUFFER_BYTES: '67108865' })).toThrow();
  });
});
