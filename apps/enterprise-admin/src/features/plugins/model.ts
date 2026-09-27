import type { PluginRecord, PluginVersionSummary } from '../../request/openapi';

export const pluginDetailPath = (name: string) => `/plugins/detail?name=${encodeURIComponent(name)}`;

export function publicationLabel(value: PluginRecord['publication']) {
  switch (value) {
    case 'unpublished':
      return '未上架';
    case 'listed':
      return '在架';
    case 'deprecated':
      return '已下架';
  }
}

export function platformLabel(version: PluginVersionSummary) {
  return version.platforms.map((item) => `${item.os}-${item.arch}`).join('、');
}

export function versionCompare(left: string, right: string) {
  const a = left.split('.').map(Number),
    b = right.split('.').map(Number);
  for (let index = 0; index < 3; index++) {
    if (a[index] !== b[index]) return (b[index] ?? 0) - (a[index] ?? 0);
  }
  return 0;
}
