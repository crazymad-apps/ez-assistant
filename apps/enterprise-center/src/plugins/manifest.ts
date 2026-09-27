import { parseDocument } from 'yaml';
import { isIP } from 'node:net';
import { invalidPackage } from './errors.js';

export type Platform = { os: 'macos' | 'windows' | 'linux'; arch: 'x64' | 'arm64' };
export type PluginManifest = {
  format_version: 1;
  name: string;
  display_name: string;
  description: string;
  version: string;
  platforms: Platform[];
  min_host_version: string;
  skills: string[];
  mcp: string[];
  configuration: { name: string; label: string; required: boolean; secret: boolean }[];
  external_software: {
    name: string;
    display_name: string;
    instructions: string;
    download_url?: string;
    platforms?: Platform[];
  }[];
};

export const pluginName = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const serverName = /^[a-z][a-z0-9._-]{0,63}$/;
const configName = serverName;
const stableVersion = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw invalidPackage();
  return value as Record<string, unknown>;
}

function fields(value: Record<string, unknown>, required: string[], optional: string[] = []): void {
  if (required.some(key => !(key in value)) || Object.keys(value).some(key => !required.includes(key) && !optional.includes(key)))
    throw invalidPackage();
}

function text(value: unknown, max: number, allowEmpty = false, multiline = false): string {
  if (typeof value !== 'string' || (!allowEmpty && !value.trim()) || [...value].length > max ||
      [...value].some(char => {
        const code = char.codePointAt(0)!;
        return code === 127 || code < 32 && !(multiline && (code === 10 || code === 13 || code === 9));
      }))
    throw invalidPackage();
  return value;
}

function names(value: unknown, max: number, pattern: RegExp): string[] {
  if (!Array.isArray(value) || value.length > max) throw invalidPackage();
  const seen = new Set<string>();
  return value.map(item => {
    const name = text(item, 128);
    if (!pattern.test(name) || seen.has(name)) throw invalidPackage();
    seen.add(name);
    return name;
  });
}

function version(value: unknown): string {
  const result = text(value, 80);
  if (!stableVersion.test(result) || result.split('.').some(part => !Number.isSafeInteger(Number(part)))) throw invalidPackage();
  return result;
}

function platform(value: unknown): Platform {
  const item = record(value);
  fields(item, ['os', 'arch']);
  if (!['macos', 'windows', 'linux'].includes(String(item.os)) || !['x64', 'arm64'].includes(String(item.arch)))
    throw invalidPackage();
  return item as Platform;
}

function platforms(value: unknown, max: number): Platform[] {
  if (!Array.isArray(value) || !value.length || value.length > max) throw invalidPackage();
  const result = value.map(platform);
  if (new Set(result.map(item => `${item.os}-${item.arch}`)).size !== result.length) throw invalidPackage();
  return result;
}

/** 清单是包的平台与能力权威声明；严格拒绝未知字段，避免中心和安装端按不同版本解释。 */
export function parseManifest(input: Buffer): PluginManifest {
  if (input.length > 1024 * 1024) throw invalidPackage();
  let raw: Record<string, unknown>;
  try { raw = record(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(input))); }
  catch { throw invalidPackage(); }
  fields(raw, ['format_version', 'name', 'display_name', 'description', 'version', 'platforms',
    'min_host_version', 'skills', 'mcp', 'configuration', 'external_software']);
  if (raw.format_version !== 1) throw invalidPackage();
  const name = text(raw.name, 64);
  if (!pluginName.test(name)) throw invalidPackage();
  const skillNames = names(raw.skills, 256, pluginName);
  if (skillNames.some(name => Buffer.byteLength(name) > 64)) throw invalidPackage();
  const mcpNames = names(raw.mcp, 256, serverName);
  if (!skillNames.length && !mcpNames.length) throw invalidPackage();
  if (!Array.isArray(raw.configuration) || raw.configuration.length > 128 ||
      !Array.isArray(raw.external_software) || raw.external_software.length > 128) throw invalidPackage();
  const configuration = raw.configuration.map(item => {
    const value = record(item);
    fields(value, ['name', 'label', 'required', 'secret']);
    const key = text(value.name, 128);
    if (!configName.test(key) || typeof value.required !== 'boolean' || typeof value.secret !== 'boolean') throw invalidPackage();
    return { name: key, label: text(value.label, 128), required: value.required, secret: value.secret };
  });
  if (new Set(configuration.map(item => item.name)).size !== configuration.length) throw invalidPackage();
  const declaredPlatforms = platforms(raw.platforms, 6);
  const externalSoftware = raw.external_software.map(item => {
    const value = record(item);
    fields(value, ['name', 'display_name', 'instructions'], ['download_url', 'platforms']);
    const key = text(value.name, 64);
    if (!pluginName.test(key)) throw invalidPackage();
    let download_url: string | undefined;
    if (value.download_url !== undefined) {
      download_url = text(value.download_url, 2048);
      try { if (!['http:', 'https:'].includes(new URL(download_url).protocol)) throw invalidPackage(); }
      catch { throw invalidPackage(); }
    }
    const scope = value.platforms === undefined ? undefined : platforms(value.platforms, 6);
    if (scope?.some(p => !declaredPlatforms.some(q => q.os === p.os && q.arch === p.arch))) throw invalidPackage();
    return { name: key, display_name: text(value.display_name, 128),
      instructions: text(value.instructions, 4096, true, true), download_url, platforms: scope };
  });
  if (new Set(externalSoftware.map(item => item.name)).size !== externalSoftware.length) throw invalidPackage();
  return {
    format_version: 1, name, display_name: text(raw.display_name, 128),
    description: text(raw.description, 4096, true, true), version: version(raw.version),
    platforms: declaredPlatforms, min_host_version: version(raw.min_host_version),
    skills: skillNames, mcp: mcpNames, configuration, external_software: externalSoftware,
  };
}

/** Skill 仅检查受限 frontmatter 与声明名称，正文留在原包中，中心不执行其指令。 */
export function validateSkill(input: Buffer, expected: string): void {
  if (input.length > 256 * 1024) throw invalidPackage();
  let content: string;
  try { content = new TextDecoder('utf-8', { fatal: true }).decode(input); }
  catch { throw invalidPackage(); }
  const match = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(content);
  if (!match || Buffer.byteLength(match[1]!) > 32 * 1024) throw invalidPackage();
  const document = parseDocument(match[1]!, { uniqueKeys: true, customTags: [] });
  if (document.errors.length) throw invalidPackage();
  let data: Record<string, unknown>;
  try { data = record(document.toJS({ maxAliasCount: 0 })); }
  catch { throw invalidPackage(); }
  if (data.name !== expected || typeof data.description !== 'string' ||
      !data.description.trim() || [...data.description].length > 1024) throw invalidPackage();
}

/** 逐平台核对入口、占位符和运行时文件；本地命令与远程 URL 互斥，配置引用必须已声明。 */
export function validateMcp(input: Buffer, expected: string, platform: Platform, manifest: PluginManifest,
  fileMode: (path: string) => number | undefined, hasDirectory: (path: string) => boolean): void {
  if (input.length > 256 * 1024) throw invalidPackage();
  let wrapper: Record<string, unknown>;
  try { wrapper = record(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(input))); }
  catch { throw invalidPackage(); }
  fields(wrapper, ['mcpServers']);
  const servers = record(wrapper.mcpServers);
  if (Object.keys(servers).length !== 1 || !(expected in servers)) throw invalidPackage();
  const server = record(servers[expected]);
  fields(server, [], ['command', 'args', 'cwd', 'env', 'url', 'headers', 'transport',
    'type', 'startupTimeoutMs', 'toolTimeoutMs', 'displayName', 'description']);
  if (server.displayName !== undefined) text(server.displayName, 128);
  if (server.description !== undefined) text(server.description, 4096, true, true);
  for (const [key, max] of [['startupTimeoutMs', 60000], ['toolTimeoutMs', 3600000]] as const) {
    const value = server[key];
    if (value !== undefined && (!Number.isSafeInteger(value) || (value as number) < 1 || (value as number) > max))
      throw invalidPackage();
  }
  if ((typeof server.command === 'string') === (typeof server.url === 'string')) throw invalidPackage();
  for (const hint of [server.transport, server.type]) {
    if (hint !== undefined && !((typeof server.command === 'string' ? ['local', 'stdio'] :
      ['remote', 'http', 'streamable-http', 'streamable_http']).includes(String(hint)))) throw invalidPackage();
  }
  // 占位符按使用位置限权，不能把配置秘密拼进命令、URL 或工作目录。
  const references = (value: string, usage: 'command' | 'args' | 'cwd' | 'env' | 'headers' | 'url') => {
    for (const match of value.matchAll(/\$\{([^}]+)\}/g)) {
      const key = match[1]!;
      if (key === 'plugin_root' && !['command', 'args', 'cwd', 'env'].includes(usage)) throw invalidPackage();
      else if (key.startsWith('config.') && (!['env', 'headers'].includes(usage) ||
        !manifest.configuration.some(item => item.name === key.slice(7)))) throw invalidPackage();
      else if (key.startsWith('software.') && (!['command', 'args', 'env'].includes(usage) ||
        !manifest.external_software.some(item => item.name === key.slice(9)))) throw invalidPackage();
      else if (key !== 'plugin_root' && !key.startsWith('config.') && !key.startsWith('software.')) throw invalidPackage();
    }
    if (value.replace(/\$\{[^}]+\}/g, '').includes('${')) throw invalidPackage();
  };
  if (typeof server.command === 'string') {
    const command = text(server.command, 4096);
    if (Buffer.byteLength(command) > 4096) throw invalidPackage();
    references(command, 'command');
    // 包内命令必须确实存在；非 Windows 平台还要具有执行位，不能借用其他平台的运行时。
    if (command.startsWith('${plugin_root}/')) {
      const target = command.slice('${plugin_root}/'.length);
      const mode = fileMode(target);
      if (mode === undefined || (platform.os !== 'windows' && (mode & 0o111) === 0) ||
        (target.startsWith('runtimes/') &&
        !target.startsWith(`runtimes/${platform.os}-${platform.arch}/`))) throw invalidPackage();
    } else if (!/^\$\{software\.[a-z0-9-]+\}$/.test(command)) throw invalidPackage();
    if (server.url !== undefined || server.headers !== undefined) throw invalidPackage();
    if (server.cwd !== undefined) {
      const cwd = text(server.cwd, 2048);
      references(cwd, 'cwd');
      if (!cwd.startsWith('${plugin_root}/') || !hasDirectory(cwd.slice('${plugin_root}/'.length))) throw invalidPackage();
    }
  } else {
    const url = text(server.url, 2048);
    references(url, 'url');
    try {
      const parsed = new URL(url);
      const host = parsed.hostname.replace(/^\[|\]$/g, '');
      // 远程服务默认必须 HTTPS；仅显式本机 loopback 可使用 HTTP。
      const loopback = host === 'localhost' || isIP(host) === 4 && host.startsWith('127.') ||
        isIP(host) === 6 && host === '::1';
      if (parsed.username || parsed.password || parsed.hash ||
        !(parsed.protocol === 'https:' || (parsed.protocol === 'http:' && loopback))) throw invalidPackage();
    }
    catch { throw invalidPackage(); }
    if (server.args !== undefined || server.cwd !== undefined || server.env !== undefined) throw invalidPackage();
  }
  if (server.args !== undefined) {
    if (!Array.isArray(server.args) || server.args.length > 64) throw invalidPackage();
    for (const item of server.args) {
      const value = text(item, 4096, true);
      references(value, 'args');
      if (value.startsWith('${plugin_root}/') && fileMode(value.slice('${plugin_root}/'.length)) === undefined)
        throw invalidPackage();
    }
  }
  for (const [key, usage] of [['env', 'env'], ['headers', 'headers']] as const) {
    if (server[key] === undefined) continue;
    const values = record(server[key]);
    if (Object.keys(values).length > 64) throw invalidPackage();
    for (const [name, value] of Object.entries(values)) {
      if (usage === 'env' ? !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) :
        !/^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/.test(name) ||
        ['host', 'content-length', 'accept', 'content-type'].includes(name.toLowerCase()) ||
        name.toLowerCase().startsWith('mcp-')) throw invalidPackage();
      references(text(value, 4096, true), usage);
    }
  }
}
