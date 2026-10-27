import MarkdownIt from 'markdown-it';
import type Token from 'markdown-it/lib/token.mjs';
import { createHash } from 'node:crypto';
import type { SiteNode } from '../contracts/site-content.js';
import { invalid } from './validation.js';

const parser = new MarkdownIt({ html: true, linkify: false });
Object.assign(parser.options, { maxNesting: 32 });
// 让所有显式链接进入统一白名单校验；不能依赖解析器将危险链接退化为普通文本。
parser.validateLink = () => true;
const tags = new Set(['paragraph', 'strong', 'em', 's', 'blockquote', 'bullet_list', 'ordered_list', 'list_item', 'table', 'thead', 'tbody', 'tr', 'th', 'td']);
export function markdown(source: string): SiteNode[] {
  let count = 0;
  function convert(tokens: Token[]): SiteNode[] {
    const root: SiteNode[] = [], stack = [root];
    for (const token of tokens) {
      if (++count > 20000 || stack.length > 32) invalid();
      const children = stack.at(-1)!;
      if (token.type === 'inline') { children.push(...convert(token.children ?? [])); continue; }
      if (token.nesting === -1) { if (stack.length < 2) invalid(); stack.pop(); continue; }
      let node: SiteNode;
      if (token.type === 'text') node = { kind: 'text', text: token.content };
      else if (token.type === 'softbreak' || token.type === 'hardbreak') node = { kind: 'break' };
      else if (token.type === 'fence' || token.type === 'code_block') {
        node = { kind: 'code', text: token.content };
        // 仅保留 fence 的语言，不把标题、属性或任意 info 字符串传给代码阅读器。
        const language = token.info.trim().split(/\s+/, 1)[0] ?? '';
        if (/^[a-zA-Z0-9][a-zA-Z0-9_+.-]{0,63}$/.test(language)) node.language = language;
      }
      else if (token.type === 'code_inline') node = { kind: 'code_inline', text: token.content };
      else if (token.type === 'hr') node = { kind: 'hr' };
      else if (token.type === 'image') node = { kind: 'image', href: token.attrGet('src') ?? '', text: token.content };
      else if (token.type === 'link_open') node = { kind: 'link', href: token.attrGet('href') ?? '', children: [] };
      else if (token.type === 'heading_open') node = { kind: 'heading', level: Number(token.tag.slice(1)), children: [] };
      else if (token.nesting === 1 && tags.has(token.type.replace(/_open$/, ''))) {
        node = { kind: token.type.replace(/_open$/, '') as SiteNode['kind'], children: [] };
        if (node.kind === 'ordered_list') node.start = Number(token.attrGet('start') ?? 1);
      } else return invalid();
      children.push(node);
      if (token.nesting === 1) stack.push(node.children!);
    }
    if (stack.length !== 1) invalid();
    return root;
  }
  const result = convert(parser.parse(source, {}));
  const ids = new Set<string>();
  walk(result, node => {
    if (node.kind !== 'heading') return;
    const last = node.children?.at(-1);
    const explicit = last?.kind === 'text' ? /\s*\{#([a-z0-9]+(?:-[a-z0-9]+)*)\}$/.exec(last.text ?? '') : null;
    if (explicit) last!.text = last!.text!.slice(0, explicit.index);
    const title = plain(node.children ?? []);
    node.id = explicit?.[1] ?? `h-${createHash('sha256').update(title).digest('hex').slice(0, 12)}`;
    if (ids.has(node.id)) invalid();
    ids.add(node.id);
  });
  return result;
}
export function walk(nodes: SiteNode[], visit: (node: SiteNode) => void): void {
  for (const node of nodes) { visit(node); if (node.children) walk(node.children, visit); }
}
export function plain(nodes: SiteNode[]): string { return nodes.map(node => node.text ?? plain(node.children ?? [])).join(''); }

/** 既有 notes 允许任意文字；官网安全降级为转义文本，不改写持久化正文或升级协议。 */
export function releaseNotes(source: string): SiteNode[] {
  try {
    const nodes = markdown(source);
    walk(nodes, node => {
      if (node.kind === 'image') {
        node.kind = 'text'; node.text = `[图片：${node.text ?? ''}]`; delete node.href;
      } else if (node.kind === 'link') {
        const href = node.href ?? '';
        const local = /^(?:\/(?:download|changelog|manual(?:\/[a-z0-9]+(?:-[a-z0-9]+)*)?)?)?(?:#[a-zA-Z0-9.-]+)?$/.test(href);
        let safe = local && !!href;
        try {
          const url = new URL(href);
          safe = ['http:', 'https:'].includes(url.protocol) && !url.username && !url.password;
        } catch { /* 站内链接没有 URL origin，按上面的固定路径白名单处理。 */ }
        // 控制字符必须显式排除，不能交给浏览器宽松归一化。
        // oxlint-disable-next-line no-control-regex
        if (!safe || /[\s\u0000-\u001f\\]/.test(href)) {
          node.kind = 'text'; node.text = plain(node.children ?? []); delete node.children; delete node.href;
        }
      }
    });
    return nodes;
  } catch {
    return [{ kind: 'paragraph', children: [{ kind: 'text', text: source }] }];
  }
}
