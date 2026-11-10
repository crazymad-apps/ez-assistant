import { parseDocument } from 'yaml';
import { isIP } from 'node:net';
import { Ajv2020 } from 'ajv/dist/2020.js';
import { invalidPackage } from './errors.js';

export type Platform = { os: 'macos' | 'windows' | 'linux'; arch: 'x64' | 'arm64' };
export type PluginManifest = {
  format_version: 1 | 2;
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
  ui?: { entries: PluginUiEntry[] };
};

export type PluginUiEntry = {
  id: string;
  title: string;
  kind: 'card' | 'app';
  resource: { type: 'package'; path: string } | { type: 'mcp'; server: string; uri: string };
  icon?: string;
  input_schema: Record<string, unknown>;
  capabilities: string[];
  csp: Record<'connectDomains' | 'resourceDomains' | 'frameDomains' | 'baseUriDomains', string[]>;
  snapshot_schema?: string;
  readable_snapshot_schemas: string[];
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
    'min_host_version', 'skills', 'mcp', 'configuration', 'external_software'], ['ui']);
  if (raw.format_version !== 1 && raw.format_version !== 2 ||
      raw.format_version === 1 && raw.ui !== undefined || raw.format_version === 2 && raw.ui === undefined)
    throw invalidPackage();
  const name = text(raw.name, 64);
  if (!pluginName.test(name)) throw invalidPackage();
  const skillNames = names(raw.skills, 256, pluginName);
  if (skillNames.some(name => Buffer.byteLength(name) > 64)) throw invalidPackage();
  const mcpNames = names(raw.mcp, 256, serverName);
  const ui = raw.ui === undefined ? undefined : parseUi(raw.ui, mcpNames);
  const minimum = version(raw.min_host_version);
  const [major, minor] = minimum.split('.').map(Number);
  if (raw.format_version === 2 && major === 0 && minor! < 33 || !skillNames.length && !mcpNames.length && !ui)
    throw invalidPackage();
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
    format_version: raw.format_version, name, display_name: text(raw.display_name, 128),
    description: text(raw.description, 4096, true, true), version: version(raw.version),
    platforms: declaredPlatforms, min_host_version: minimum,
    skills: skillNames, mcp: mcpNames, configuration, external_software: externalSoftware,
    ...(ui ? { ui } : {}),
  };
}

const uiIdentifier = /^[A-Za-z0-9._-]{1,64}$/;
const uiCapabilities = ['save-snapshot', 'open-app', 'post-message', 'set-composer-draft', 'call-tool'];
const cspFields = ['connectDomains', 'resourceDomains', 'frameDomains', 'baseUriDomains'] as const;

function uiPath(value: unknown): string {
  const path = text(value, 1024);
  if (Buffer.byteLength(path) > 1024 || !path.startsWith('resources/') || path.includes('\\') || /[\u0000-\u001f\u007f-\u009f]/.test(path) ||
      path.split('/').some(part => !part || part === '.' || part === '..')) throw invalidPackage();
  return path;
}

/** UI 只接受离线 2020-12 Schema；编译不读取远端引用，预算与 Host 的现有编译器一致。 */
function uiSchema(input: unknown): Record<string, unknown> {
  const schema = record(input);
  if (Buffer.byteLength(JSON.stringify(schema)) > 128 * 1024 || schema.$schema !== undefined &&
      schema.$schema !== 'https://json-schema.org/draft/2020-12/schema') throw invalidPackage();
  let nodes = 0;
  const visit = (value: unknown, depth: number): void => {
    if (++nodes > 4096 || depth > 64) throw invalidPackage();
    if (Array.isArray(value)) { value.forEach(item => visit(item, depth + 1)); return; }
    if (!value || typeof value !== 'object') return;
    for (const [key, item] of Object.entries(value)) {
      if (['$ref', '$dynamicRef', '$recursiveRef'].includes(key) &&
          (typeof item !== 'string' || !item.startsWith('#'))) throw invalidPackage();
      if (key === 'pattern' && typeof item === 'string' && Buffer.byteLength(item) > 4096 ||
          key === 'patternProperties' && item && typeof item === 'object' &&
          Object.keys(item).some(pattern => Buffer.byteLength(pattern) > 4096)) throw invalidPackage();
      visit(item, depth + 1);
    }
  };
  visit(schema, 1);
  try { new Ajv2020({ strict: false, validateFormats: false, logger: false }).compile(schema); }
  catch { throw invalidPackage(); }
  return schema;
}

function parseUi(input: unknown, servers: string[]): { entries: PluginUiEntry[] } {
  const ui = record(input);
  fields(ui, ['entries']);
  if (!Array.isArray(ui.entries) || !ui.entries.length || ui.entries.length > 128) throw invalidPackage();
  const ids = new Set<string>();
  const entries = ui.entries.map((raw): PluginUiEntry => {
    const item = record(raw);
    fields(item, ['id', 'title', 'kind', 'resource', 'input_schema'],
      ['icon', 'capabilities', 'csp', 'snapshot_schema', 'readable_snapshot_schemas']);
    const id = text(item.id, 64);
    if (!uiIdentifier.test(id) || ids.has(id) || item.kind !== 'card' && item.kind !== 'app') throw invalidPackage();
    ids.add(id);
    const resource = record(item.resource);
    let target: PluginUiEntry['resource'];
    if (resource.type === 'package') {
      fields(resource, ['type', 'path']);
      const path = uiPath(resource.path);
      if (!path.endsWith('.html')) throw invalidPackage();
      target = { type: 'package', path };
    } else {
      fields(resource, ['type', 'server', 'uri']);
      if (resource.type !== 'mcp' || typeof resource.server !== 'string' || !servers.includes(resource.server)) throw invalidPackage();
      const uri = text(resource.uri, 1024);
      if (!uri.startsWith('ui://') || Buffer.byteLength(uri) > 1024 || /[\u007f-\u009f]/.test(uri)) throw invalidPackage();
      target = { type: 'mcp', server: resource.server, uri };
    }
    const capabilities = names(item.capabilities === undefined ? [] : item.capabilities, 5, /^(save-snapshot|open-app|post-message|set-composer-draft|call-tool)$/);
    if (capabilities.some(capability => !uiCapabilities.includes(capability))) throw invalidPackage();
    const snapshot_schema = item.snapshot_schema == null ? undefined : text(item.snapshot_schema, 64);
    const readable = names(item.readable_snapshot_schemas === undefined ? [] : item.readable_snapshot_schemas, 32, uiIdentifier);
    if (item.kind === 'card' && (!snapshot_schema || !uiIdentifier.test(snapshot_schema)) ||
        item.kind === 'app' && (snapshot_schema !== undefined || readable.length > 0)) throw invalidPackage();
    const csp = record(item.csp === undefined ? {} : item.csp);
    // 与标准 MCP CSP 一样，未知扩展不赋予任何网络能力。
    const projected = {} as PluginUiEntry['csp'];
    for (const key of cspFields) {
      if (csp[key] !== undefined && (!Array.isArray(csp[key]) || csp[key].length > 32)) throw invalidPackage();
      projected[key] = (csp[key] ?? [] as unknown[]) as string[];
      for (const value of projected[key]) {
        if (typeof value !== 'string' || value.length > 2048 || /[^\x20-\x7e]|['";*]/.test(value)) throw invalidPackage();
        let origin: URL;
        try { origin = new URL(value); } catch { throw invalidPackage(); }
        if (origin.protocol !== 'https:' && !(key === 'connectDomains' && origin.protocol === 'wss:') ||
            origin.username || origin.password || origin.pathname !== '/' || origin.search || origin.hash || origin.origin !== value)
          throw invalidPackage();
      }
    }
    const title = text(item.title, 128);
    if (/[\u007f-\u009f]/.test(title)) throw invalidPackage();
    return { id, title, kind: item.kind, resource: target,
      ...(item.icon == null ? {} : { icon: uiPath(item.icon) }),
      input_schema: uiSchema(item.input_schema), capabilities, csp: projected,
      ...(snapshot_schema === undefined ? {} : { snapshot_schema }), readable_snapshot_schemas: readable };
  });
  return { entries };
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
