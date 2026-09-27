import type { AgentCapabilityReference, AgentDependencyDiagnostic, PluginRecord } from '../../request/openapi';

export const agentEditPath = (name: string) => `/agents/edit?name=${encodeURIComponent(name)}`;

export function agentNameError(name: string): string | undefined {
  if (
    !name ||
    [...name].length > 64 ||
    name.normalize('NFC') !== name ||
    name.trim() !== name ||
    [...name].some((char) => {
      const code = char.codePointAt(0)!;
      return code < 32 || (code >= 127 && code <= 159) || (code >= 0xd800 && code <= 0xdfff);
    })
  )
    return '名称须为 1—64 个 NFC 字符，不能有首尾空白或控制字符。';
}

export function capabilityNameValid(kind: 'skills' | 'mcp', name: string) {
  const pattern = kind === 'skills' ? /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/ : /^[a-z][a-z0-9._-]{0,63}$/;
  return name.length <= 64 && pattern.test(name);
}

export function capabilityChoices(plugins: PluginRecord[], plugin: string, kind: 'skills' | 'mcp') {
  const record = plugins.find((item) => item.name === plugin);
  if (!record) return [];
  return [...new Set(record.versions.flatMap((version) => version[kind]))].sort();
}

export function dependencyLabel(item: AgentDependencyDiagnostic) {
  switch (item.status) {
    case 'available':
      return `中心包已声明；平台：${item.platforms.map((p) => `${p.os}-${p.arch}`).join('、')}`;
    case 'host_check_required':
      return '需在目标 Host 核对';
    case 'plugin_missing':
      return '所属插件不存在';
    case 'plugin_unpublished':
      return '所属插件未上架';
    case 'plugin_deprecated':
      return '所属插件已下架';
    case 'capability_missing':
      return '当前插件版本均未声明此能力';
  }
}

export function sameReference(a: AgentCapabilityReference, b: AgentCapabilityReference) {
  return a.name === b.name && a.plugin === b.plugin;
}
