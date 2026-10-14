import type { SiteNode } from '../request/openapi';

export function nodeText(node: SiteNode): string {
  return node.text ?? (node.children ?? []).map(nodeText).join('');
}

/** Markdown 二级标题作为展示卡片边界，正文仍由统一安全阅读器渲染。 */
export function documentSections(body: SiteNode[]): { title: string; id: string; body: SiteNode[] }[] {
  const sections: { title: string; id: string; body: SiteNode[] }[] = [];
  for (const node of body) {
    if (node.kind === 'heading' && node.level === 2)
      sections.push({ title: nodeText(node), id: node.id ?? '', body: [] });
    else {
      if (!sections.length) sections.push({ title: '', id: '', body: [] });
      sections[sections.length - 1]!.body.push(node);
    }
  }
  return sections;
}
