import { expect, it } from 'vitest';
import { agentEtag, agentInput, agentName, diagnoseAgent, requireAgentEtag } from '../../dist/agents/catalog.js';
import type { AgentConfig } from '../../dist/contracts/agents.js';
import type { PluginEntity, PluginPackageEntity } from '../../dist/plugins/entity.js';

const config = (): AgentConfig => ({
  name: '文档审阅助手', description: '团队文档', prompt: '  保留空格\n并完整审阅。\n',
  skills: [{ name: 'document-review', plugin: 'document-tools' }],
  mcp: [{ name: 'local.server' }],
});

function packageRow(name: string, skills: string[], platforms: { os: string; arch: string }[]) {
  return { plugin_name: name, manifest: { skills, mcp: [], platforms } } as unknown as PluginPackageEntity;
}

it('Agent 名称要求 NFC、无首尾空白且最多 64 个 Unicode 字符', () => {
  expect(agentName('文档审阅助手')).toBe('文档审阅助手');
  for (const value of [' 文档', '文档\n', 'e\u0301', 'a'.repeat(65), '文\u0000档'])
    expect(() => agentName(value)).toThrow();
});

it('完整提示词保留原始空白、按 UTF-8 字节限制，纯提示词 Agent 有效', () => {
  const input = agentInput({ ...config(), skills: [], mcp: [] });
  expect(input.prompt).toBe(config().prompt);
  expect(input.skills).toEqual([]);
  expect(agentInput({ ...config(), prompt: '中'.repeat(87381) }).prompt.length).toBe(87381);
  expect(() => agentInput({ ...config(), prompt: '中'.repeat(87382) })).toThrow();
  expect(() => agentInput({ ...config(), prompt: ' \n\t ' })).toThrow();
});

it('同类名称跨来源仍不允许重复，未知结构和非法能力名拒绝', () => {
  expect(() => agentInput({ ...config(), skills: [
    { name: 'document-review', plugin: 'one' }, { name: 'document-review', plugin: 'two' },
  ] })).toThrow();
  expect(() => agentInput({ ...config(), mcp: [{ name: 'BadName' }] })).toThrow();
  expect(() => agentInput({ ...config(), unexpected: true })).toThrow();
});

it('完整配置 ETag 覆盖提示词与引用来源，旧表单条件写失败', () => {
  const original = config(), etag = agentEtag(original);
  expect(agentEtag({ ...original })).toBe(etag);
  expect(agentEtag({ ...original, prompt: original.prompt + '补充' })).not.toBe(etag);
  expect(agentEtag({ ...original, skills: [{ name: 'document-review', plugin: 'another' }] })).not.toBe(etag);
  expect(() => requireAgentEtag(undefined, etag)).toThrow();
  expect(() => requireAgentEtag('"stale"', etag)).toThrow();
  expect(() => requireAgentEtag(etag, etag)).not.toThrow();
});

it('诊断明确区分 Host 待核对、缺插件、废弃与能力/平台事实', () => {
  const agent = config();
  agent.skills.push({ name: 'missing-skill', plugin: 'document-tools' });
  agent.skills.push({ name: 'other-skill', plugin: 'absent-plugin' });
  const plugin = { name: 'document-tools', publication: 'listed' } as PluginEntity;
  const packages = [packageRow('document-tools', ['document-review'], [{ os: 'macos', arch: 'arm64' }])];
  const diagnostic = diagnoseAgent(agent, [plugin], packages);
  expect(diagnostic.map(item => item.status)).toEqual([
    'available', 'capability_missing', 'plugin_missing', 'host_check_required',
  ]);
  expect(diagnostic[0]!.platforms).toEqual([{ os: 'macos', arch: 'arm64' }]);
  expect(diagnostic[1]!.platforms).toEqual([]);
  expect(diagnoseAgent(agent, [{ ...plugin, publication: 'deprecated' }], packages)[0]!.status)
    .toBe('plugin_deprecated');
});
