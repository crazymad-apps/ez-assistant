import { createHash } from 'node:crypto';
import { fields, invalid } from '../validation.js';
import type { AgentCapabilityReference, AgentConfig, AgentDependencyDiagnostic } from '../contracts/agents.js';
import type { CustomAgentEntity } from './entity.js';
import type { PluginEntity, PluginPackageEntity } from '../plugins/entity.js';
import { pluginName } from '../plugins/manifest.js';
import { AgentError } from './errors.js';

const mcpName = /^[a-z][a-z0-9._-]{0,63}$/;
type ReferenceKind = 'skill' | 'mcp';

function forbiddenCharacter(value: string, multiline: boolean): boolean {
  return [...value].some(char => {
    const code = char.codePointAt(0)!;
    // 未配对代理项会在 UTF-8 编码时被替换，不能作为数据库名称或正文原样回读。
    if (code >= 0xd800 && code <= 0xdfff) return true;
    return code < 32 && !(multiline && [9, 10, 13].includes(code)) || code >= 127 && code <= 159;
  });
}

/** Agent 名称是精确身份；拒绝非 NFC 与首尾空白，避免视觉相同但数据库键不同。 */
export function agentName(value: unknown): string {
  if (typeof value !== 'string' || !value || [...value].length > 64 ||
      value.normalize('NFC') !== value || value.trim() !== value || forbiddenCharacter(value, false)) invalid();
  return value;
}

function description(value: unknown): string {
  if (typeof value !== 'string' || [...value].length > 4096 || forbiddenCharacter(value, true)) invalid();
  return value;
}

function prompt(value: unknown): string {
  if (typeof value !== 'string' || !value.trim() || Buffer.byteLength(value, 'utf8') > 256 * 1024 ||
      forbiddenCharacter(value, true)) invalid();
  return value;
}

function references(value: unknown, kind: ReferenceKind): AgentCapabilityReference[] {
  if (!Array.isArray(value) || value.length > 256) invalid();
  const seen = new Set<string>();
  return value.map(item => {
    const input = fields(item, ['name'], ['plugin']);
    const name = input.name;
    if (typeof name !== 'string' || !name || Buffer.byteLength(name) > 64 ||
        !(kind === 'skill' ? pluginName : mcpName).test(name) || seen.has(name)) invalid();
    seen.add(name);
    if (input.plugin === undefined) return { name };
    if (typeof input.plugin !== 'string' || !input.plugin || Buffer.byteLength(input.plugin) > 64 ||
        !pluginName.test(input.plugin)) invalid();
    return { name, plugin: input.plugin };
  });
}

export function agentInput(value: unknown): AgentConfig {
  const input = fields(value, ['name', 'description', 'prompt', 'skills', 'mcp']);
  return { name: agentName(input.name), description: description(input.description), prompt: prompt(input.prompt),
    skills: references(input.skills, 'skill'), mcp: references(input.mcp, 'mcp') };
}

/** ETag 仅覆盖完整配置字段；数组顺序与来源都属于用户保存的配置事实。 */
export function agentEtag(row: Pick<AgentConfig, 'name' | 'description' | 'prompt' | 'skills' | 'mcp'>): string {
  const references = (items: AgentCapabilityReference[]) => items.map(item => [item.name, item.plugin ?? null]);
  return `"${createHash('sha256').update(JSON.stringify([
    row.name, row.description, row.prompt, references(row.skills), references(row.mcp),
  ])).digest('hex')}"`;
}

export function requireAgentEtag(provided: string | undefined, current: string): void {
  if (!provided) throw new AgentError(428, 'PRECONDITION_REQUIRED');
  if (provided !== current) throw new AgentError(412, 'CONFIG_CHANGED');
}

export function agentConfig(row: CustomAgentEntity): AgentConfig {
  return { name: row.name, description: row.description, prompt: row.prompt,
    skills: row.skills, mcp: row.mcp };
}

/** 诊断只从当前包清单推导中心可知事实；无插件来源不猜测 Host 安装/权限。 */
export function diagnoseAgent(config: AgentConfig, plugins: PluginEntity[],
  packages: PluginPackageEntity[]): AgentDependencyDiagnostic[] {
  const known = new Map(plugins.map(plugin => [plugin.name, plugin]));
  const diagnose = (kind: ReferenceKind, reference: AgentCapabilityReference): AgentDependencyDiagnostic => {
    const base = { kind, name: reference.name, ...(reference.plugin ? { plugin: reference.plugin } : {}) };
    if (!reference.plugin) return { ...base, status: 'host_check_required', platforms: [] };
    const plugin = known.get(reference.plugin);
    if (!plugin) return { ...base, status: 'plugin_missing', platforms: [] };
    const matches = packages.filter(row => row.plugin_name === plugin.name &&
      row.manifest[kind === 'skill' ? 'skills' : 'mcp'].includes(reference.name));
    const platforms = [...new Map(matches.flatMap(row => row.manifest.platforms).map(item =>
      [`${item.os}-${item.arch}`, item] as const)).values()].sort((a, b) =>
      `${a.os}-${a.arch}`.localeCompare(`${b.os}-${b.arch}`));
    if (plugin.publication === 'unpublished') return { ...base, status: 'plugin_unpublished', platforms };
    if (plugin.publication === 'deprecated') return { ...base, status: 'plugin_deprecated', platforms };
    if (!matches.length) return { ...base, status: 'capability_missing', platforms: [] };
    return { ...base, status: 'available', platforms };
  };
  return [...config.skills.map(item => diagnose('skill', item)), ...config.mcp.map(item => diagnose('mcp', item))];
}
