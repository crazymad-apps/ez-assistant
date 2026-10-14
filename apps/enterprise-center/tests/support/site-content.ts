/** 只用于隔离验证，不进入 docs/site 发布清单。 */
export function siteFixture(version = '0.31.0', label = '测试内容') {
  const manifest = {
    format: 1,
    home: [{ kind: 'hero', title: label, description: '隔离验证', file: 'home/hero.md' }],
    manual: [{ title: '入门', articles: [{ id: 'installation', file: 'manual/installation.md' }] }],
    assets: [{ id: 'sample', file: 'assets/sample.png' }],
  };
  return new Map<string, Buffer>([
    ['site.json', Buffer.from(JSON.stringify(manifest))],
    ['home/hero.md', Buffer.from(`## ${label}\n\n[安装指南](../manual/installation.md#install)\n\n![测试图片](../assets/sample.png)\n`)],
    ['manual/installation.md', Buffer.from(`---\nid: installation\ntitle: 安装指南\nupdated: '2026-10-13'\napplies_to: ${version}\n---\n## 安装 {#install}\n\n**先选择平台**，再下载安装包。\n\n| 平台 | 操作 |\n| --- | --- |\n| 测试平台 | 测试操作 |\n\n[日志](/changelog#v${version})\n`)],
    ['assets/sample.png', Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a9K8AAAAASUVORK5CYII=', 'base64')],
  ]);
}
