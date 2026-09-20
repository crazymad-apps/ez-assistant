import { describe, expect, it } from 'vitest';
import { databaseConfig, databaseTarget, loadConfig } from '../../dist/config.js';
import { safeError } from '../../dist/errors.js';

const env = { CENTER_DATABASE_URL: 'postgres://center_app:secret@127.0.0.1:55432/center_test',
  CENTER_PUBLIC_ORIGIN: 'http://127.0.0.1:7320', CENTER_ALLOW_HTTP_LOOPBACK: 'true' };

describe('显式配置与脱敏', () => {
  it('允许显式 loopback 开发配置', () => { expect(loadConfig(env).port).toBe(7320); });
  it('数据库无默认目标', () => { expect(() => databaseConfig({})).toThrow(); });
  it('目标报告不包含密码', () => { expect(JSON.stringify(databaseTarget(databaseConfig(env)))).not.toContain('secret'); });
  it.each(['?host=elsewhere', '?dbname=other', '?options=-csearch_path=evil'])('禁止 URI 重定向 %s', suffix => {
    expect(() => databaseConfig({ ...env, CENTER_DATABASE_URL: env.CENTER_DATABASE_URL + suffix })).toThrow();
  });
  it('单一数据库连接足够，不需要额外角色配置', () => { expect(databaseConfig(env)).toEqual({ url: env.CENTER_DATABASE_URL }); });
  it('默认开启自动升级', () => { expect(loadConfig(env).autoUpgrade).toBe(true); });
  it.each(['true', 'false'])('显式自动升级开关 %s', value => {
    expect(loadConfig({ ...env, CENTER_DATABASE_AUTO_UPGRADE: value }).autoUpgrade).toBe(value === 'true');
  });
  it.each(['', 'yes', '1', 'FALSE'])('拒绝误拼的自动升级开关 %s', value => {
    expect(() => loadConfig({ ...env, CENTER_DATABASE_AUTO_UPGRADE: value })).toThrow();
  });
  it('非本地 HTTP 拒绝启动', () => { expect(() => loadConfig({ ...env, CENTER_HOST: '0.0.0.0' })).toThrow(); });
  it('拒绝未显式允许的 HTTP', () => { expect(() => loadConfig({ ...env, CENTER_ALLOW_HTTP_LOOPBACK: undefined })).toThrow(); });
  it('HTTPS origin 允许非本地显式监听', () => { expect(loadConfig({ ...env, CENTER_HOST: '0.0.0.0', CENTER_PUBLIC_ORIGIN: 'https://center.example' }).host).toBe('0.0.0.0'); });
  it.each(['0', '65536', '7320junk'])('拒绝非法端口 %s', port => { expect(() => loadConfig({ ...env, CENTER_PORT: port })).toThrow(); });
  it('未知异常不透传内容', () => { expect(JSON.stringify(safeError(new Error('password=secret SQL SELECT')))).not.toMatch(/secret|SELECT/); });
});
