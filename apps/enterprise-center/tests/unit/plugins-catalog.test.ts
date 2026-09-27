import { expect, it } from 'vitest';
import { choosePackage, compareVersions, pluginEtag, requireEtag, resolveInput } from '../../dist/plugins/catalog.js';
import type { PluginEntity, PluginPackageEntity } from '../../dist/plugins/entity.js';

function packageRow(version: string, options: { minHost?: string; skills?: string[]; os?: 'macos' | 'windows' } = {}) {
  return {
    version, sha256: version, archive_format: 'zip', size_bytes: '10',
    manifest: { platforms: [{ os: options.os ?? 'macos', arch: 'arm64' }],
      min_host_version: options.minHost ?? '0.28.0', skills: options.skills ?? ['review'], mcp: [] },
  } as unknown as PluginPackageEntity;
}

it('三段稳定版本按数字比较，不按字符串排序或接受范围', () => {
  expect(compareVersions('1.10.0', '1.9.9')).toBe(1);
  expect(compareVersions('1.2.0', '1.2.0')).toBe(0);
  expect(compareVersions('0.9.0', '1.0.0')).toBe(-1);
  expect(() => compareVersions('1.0.0-beta', '1.0.0')).toThrow();
});

it('resolve 输入只接受声明字段和不重复的能力名称', () => {
  expect(resolveInput({ os: 'macos', arch: 'arm64', host_version: '0.28.0' }).required_skills).toEqual([]);
  expect(() => resolveInput({ os: 'macos', arch: 'arm64', host_version: '0.28.0',
    required_skills: ['review', 'review'] })).toThrow();
  expect(() => resolveInput({ os: 'macos', arch: 'arm64', host_version: '0.28.0', extra: true })).toThrow();
});

it('插件 ETag 只随说明或发布状态变化，旧表单条件写被拒绝', () => {
  const row = { name: 'docs', display_name: '文档', description: '查看', publication: 'unpublished' } as PluginEntity;
  const etag = pluginEtag(row);
  expect(() => requireEtag(undefined, etag)).toThrow();
  expect(() => requireEtag('"stale"', etag)).toThrow();
  expect(() => requireEtag(etag, etag)).not.toThrow();
  expect(pluginEtag({ ...row, publication: 'listed' })).not.toBe(etag);
});

it('依次区分平台、Host、能力与包文件失败，优先选择最高可用版本', async () => {
  const input = resolveInput({ os: 'macos', arch: 'arm64', host_version: '0.28.0',
    required_skills: ['review'] });
  const available = async () => true;
  await expect(choosePackage([packageRow('1.0.0', { os: 'windows' })], input, available))
    .rejects.toMatchObject({ code: 'PLATFORM_UNSUPPORTED' });
  await expect(choosePackage([packageRow('1.0.0', { minHost: '0.29.0' })], input, available))
    .rejects.toMatchObject({ code: 'HOST_VERSION_UNSUPPORTED' });
  await expect(choosePackage([packageRow('1.0.0', { skills: [] })], input, available))
    .rejects.toMatchObject({ code: 'CAPABILITY_MISSING' });
  await expect(choosePackage([packageRow('1.0.0')], input, async () => false))
    .rejects.toMatchObject({ code: 'PACKAGE_UNAVAILABLE' });
  expect((await choosePackage([packageRow('1.9.0'), packageRow('1.10.0')], input, available)).version)
    .toBe('1.10.0');
  expect((await choosePackage([packageRow('1.9.0'), packageRow('1.10.0')], input,
    async row => row.version === '1.9.0')).version).toBe('1.9.0');
});

it('installed_version 禁止自动回退到更旧的可用版本', async () => {
  const input = resolveInput({ os: 'macos', arch: 'arm64', host_version: '0.28.0',
    installed_version: '2.0.0' });
  await expect(choosePackage([packageRow('1.9.0')], input, async () => true))
    .rejects.toMatchObject({ code: 'PACKAGE_UNAVAILABLE' });
});
