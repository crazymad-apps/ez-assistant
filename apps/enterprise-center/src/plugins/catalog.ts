import { createHash } from 'node:crypto';
import { fields, invalid } from '../validation.js';
import type { PluginEntity, PluginPackageEntity } from './entity.js';
import { PluginError } from './errors.js';
import { pluginName } from './manifest.js';

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const serverName = /^[a-z][a-z0-9._-]{0,63}$/;
export type ResolveInput = {
  os: 'macos' | 'windows' | 'linux';
  arch: 'x64' | 'arm64';
  host_version: string;
  installed_version?: string;
  required_skills: string[];
  required_mcp: string[];
};

/** 版本只有三段安全整数；不接受范围、预发布标记或宽松 SemVer 转换。 */
export function versionParts(value: unknown): number[] {
  if (typeof value !== 'string' || !versionPattern.test(value)) invalid();
  const parts = value.split('.').map(Number);
  if (parts.some(part => !Number.isSafeInteger(part))) invalid();
  return parts;
}

export function compareVersions(left: string, right: string): number {
  const a = versionParts(left), b = versionParts(right);
  for (let index = 0; index < 3; index++) {
    if (a[index]! !== b[index]!) return a[index]! > b[index]! ? 1 : -1;
  }
  return 0;
}

export function validPluginName(value: string): string {
  if (!pluginName.test(value) || Buffer.byteLength(value) > 64) invalid();
  return value;
}

/** ETag 只覆盖管理员可编辑字段与发布事实；新包上传不使说明表单失效。 */
export function pluginEtag(row: PluginEntity): string {
  const hash = createHash('sha256').update(JSON.stringify([
    row.name, row.display_name, row.description, row.publication,
  ])).digest('hex');
  return `"${hash}"`;
}

export function requireEtag(provided: string | undefined, current: string): void {
  if (!provided) throw new PluginError(428, 'PRECONDITION_REQUIRED');
  if (provided !== current) throw new PluginError(412, 'CONFIG_CHANGED');
}

function text(value: unknown, limit: number, allowEmpty = false): string {
  if (typeof value !== 'string' || (!allowEmpty && !value.trim()) ||
      [...value].length > limit || [...value].some(char => {
        const code = char.codePointAt(0)!;
        return code < 32 && ![9, 10, 13].includes(code) || code === 127;
      })) invalid();
  return value;
}

export function descriptionInput(value: unknown): { display_name: string; description: string } {
  const input = fields(value, ['display_name', 'description']);
  return { display_name: text(input.display_name, 128), description: text(input.description, 4096, true) };
}

export function publicationInput(value: unknown): boolean {
  const input = fields(value, ['listed']);
  if (typeof input.listed !== 'boolean') invalid();
  return input.listed;
}

function names(value: unknown, pattern: RegExp): string[] {
  if (!Array.isArray(value) || value.length > 256) invalid();
  const result = value.map(item => text(item, 128));
  if (result.some(item => !pattern.test(item)) || new Set(result).size !== result.length) invalid();
  return result;
}

export function resolveInput(value: unknown): ResolveInput {
  const input = fields(value, ['os', 'arch', 'host_version'],
    ['installed_version', 'required_skills', 'required_mcp']);
  if (!['macos', 'windows', 'linux'].includes(String(input.os)) ||
      !['x64', 'arm64'].includes(String(input.arch))) invalid();
  versionParts(input.host_version);
  if (input.installed_version !== undefined) versionParts(input.installed_version);
  return {
    os: input.os as ResolveInput['os'], arch: input.arch as ResolveInput['arch'],
    host_version: input.host_version as string,
    ...(input.installed_version === undefined ? {} : { installed_version: input.installed_version as string }),
    required_skills: names(input.required_skills ?? [], pluginName),
    required_mcp: names(input.required_mcp ?? [], serverName),
  };
}

export function packageSummary(row: PluginPackageEntity) {
  return {
    version: row.version, platforms: row.manifest.platforms,
    min_host_version: row.manifest.min_host_version,
    skills: row.manifest.skills, mcp: row.manifest.mcp,
    archive_format: row.archive_format, sha256: row.sha256,
    size_bytes: Number(row.size_bytes),
  };
}

/** 分层过滤保留首个可解释的失败原因；只读候选，不改变安装或已分发状态。 */
export async function choosePackage(rows: PluginPackageEntity[], input: ResolveInput,
  available: (row: PluginPackageEntity) => Promise<boolean>): Promise<PluginPackageEntity> {
  const sorted = [...rows].sort((a, b) => compareVersions(b.version, a.version));
  const platform = sorted.filter(row => row.manifest.platforms.some(
    item => item.os === input.os && item.arch === input.arch));
  if (!platform.length) throw new PluginError(409, 'PLATFORM_UNSUPPORTED');
  const host = platform.filter(row => compareVersions(input.host_version, row.manifest.min_host_version) >= 0);
  if (!host.length) throw new PluginError(409, 'HOST_VERSION_UNSUPPORTED');
  const capabilities = host.filter(row => input.required_skills.every(name => row.manifest.skills.includes(name)) &&
    input.required_mcp.every(name => row.manifest.mcp.includes(name)));
  if (!capabilities.length) throw new PluginError(409, 'CAPABILITY_MISSING');
  // 已安装版本以上无候选时不能静默降级；此时没有可交付的更新包。
  for (const row of capabilities) {
    if (input.installed_version && compareVersions(row.version, input.installed_version) < 0) continue;
    if (await available(row)) return row;
  }
  throw new PluginError(503, 'PACKAGE_UNAVAILABLE');
}
