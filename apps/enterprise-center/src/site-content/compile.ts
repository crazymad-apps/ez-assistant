import { createHash } from 'node:crypto';
import { posix } from 'node:path';
import { parseDocument } from 'yaml';
import { imageSize } from 'image-size';
import type { SiteArticle, SiteContent, SiteHomeSection, SiteNode } from '../contracts/site-content.js';
import { date, inSource, invalid, limits, list, path, record, slug, text, utf8 } from './validation.js';
import { markdown, walk } from './markdown.js';

const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
/** 清单决定静态输出集合；未选中的草稿与日志留在源目录。 */
export function sourceManifest(bytes: Buffer) {
  if (bytes.length > limits.document) invalid();
  const raw = record(JSON.parse(utf8(bytes)), ['format', 'home', 'manual', 'assets']);
  if (raw.format !== 1) invalid();
  const home = list(raw.home).map(item => {
    const row = record(item, ['kind', 'title', 'description', 'file', 'scenarios']);
    if (!['hero', 'workspace', 'capabilities', 'modes', 'resources', 'closing'].includes(String(row.kind))) invalid();
    const file = path(row.file); if (!/^home\/[\w/-]+\.md$/.test(file)) invalid();
    const scenarios = row.scenarios === undefined ? undefined : list(row.scenarios).map(value => {
      const scene = record(value, ['id', 'label', 'plugin', 'prompt', 'intro', 'title', 'body', 'file', 'reply', 'steps', 'kind']);
      if (row.kind !== 'workspace' || !['drawing', 'scene', 'quote', 'notes'].includes(String(scene.kind))) invalid();
      return { id: slug(scene.id), label: text(scene.label, 100), plugin: text(scene.plugin, 100),
        prompt: text(scene.prompt), intro: text(scene.intro), title: text(scene.title, 200), body: text(scene.body),
        file: text(scene.file, 200), reply: text(scene.reply), steps: list(scene.steps).map(step => text(step, 200)),
        kind: scene.kind as 'drawing' | 'scene' | 'quote' | 'notes' };
    });
    if (scenarios && (scenarios.length > 8 || new Set(scenarios.map(scene => scene.id)).size !== scenarios.length)) invalid();
    return { ...(scenarios ? { scenarios } : {}), kind: row.kind as SiteHomeSection['kind'], title: text(row.title, 200), description: text(row.description), file };
  });
  if (new Set(home.map(item => item.kind)).size !== home.length) invalid();
  const manual = list(raw.manual).map(item => {
    const row = record(item, ['title', 'articles']);
    return { title: text(row.title, 100), articles: list(row.articles).map(value => {
      const article = record(value, ['id', 'file']);
      const file = path(article.file); if (!/^manual\/[\w/-]+\.md$/.test(file)) invalid();
      return { id: slug(article.id), file };
    }) };
  });
  const assets = list(raw.assets).map(value => { const row = record(value, ['id', 'file']); const file = path(row.file); if (!/^assets\/[\w/-]+\.(png|jpg|jpeg|webp)$/.test(file)) invalid(); return { id: slug(row.id), file }; });
  const files = ['site.json', ...home.map(item => item.file), ...manual.flatMap(item => item.articles.map(a => a.file)), ...assets.map(item => item.file)];
  if (files.length > limits.entries || new Set(files.map(file => file.toLowerCase())).size !== files.length || new Set(assets.map(a => a.id)).size !== assets.length) invalid();
  return { home, manual, assets, files };
}
export function compile(files: Map<string, Buffer>): SiteContent {
  const manifest = inSource('site.json', '清单格式、字段或容量无效', () => sourceManifest(files.get('site.json') ?? invalid()));
  if (files.size !== manifest.files.length || manifest.files.some(file => !files.has(file))) invalid();
  let total = 0, texts = 0;
  for (const [file, bytes] of files) {
    total += bytes.length;
    if (!file.startsWith('assets/')) { texts += bytes.length; if (bytes.length > limits.document) invalid(); }
  }
  if (total > limits.total || texts > limits.text) invalid();
  const docs = new Map<string, { url: string; nodes: SiteNode[] }>();
  const articleIds = new Set<string>();
  function article(file: string, id: string): SiteArticle {
    const raw = utf8(files.get(file)!);
    const match = /^---\r?\n([\s\S]*?)\r?\n---\r?\n([\s\S]*)$/.exec(raw); if (!match) invalid();
    const yaml = parseDocument(match[1]!, { customTags: [], uniqueKeys: true });
    if (yaml.errors.length || yaml.warnings.length) invalid();
    const meta = record(yaml.toJS({ maxAliasCount: 0 }), ['id', 'title', 'updated', 'applies_to']);
    const nodes = markdown(match[2]!);
    const articleId = slug(meta.id);
    if (articleIds.has(articleId) || (id && id !== articleId)) invalid();
    articleIds.add(articleId);
    const title = text(meta.title, 200);
    docs.set(file, { url: `/manual/${articleId}`, nodes });
    return { id: articleId, title, date: date(meta.updated), applies_to: text(meta.applies_to, 200), body: nodes };
  }
  const manual = manifest.manual.map(group => ({ title: group.title, articles: group.articles.map(a => inSource(a.file, '文章元信息或 Markdown 格式无效', () => article(a.file, a.id))) }));
  const home = manifest.home.map(item => { const nodes = inSource(item.file, 'Markdown 格式无效或含不支持内容', () => markdown(utf8(files.get(item.file)!))); docs.set(item.file, { url: '/', nodes }); return { kind: item.kind, title: item.title, description: item.description, body: nodes, ...(item.scenarios ? { scenarios: item.scenarios } : {}) }; });
  const assets = manifest.assets.map(asset => inSource(asset.file, '图片类型、尺寸或容量无效', () => {
    const bytes = files.get(asset.file)!; if (bytes.length > limits.image) invalid();
    const info = imageSize(bytes), ext = posix.extname(asset.file).slice(1);
    if (!info.width || !info.height || info.width > 8192 || info.height > 8192 || !['png', 'jpg', 'webp'].includes(info.type ?? '') || (ext === 'jpeg' ? 'jpg' : ext) !== info.type) invalid();
    return { id: asset.id, type: `image/${info.type === 'jpg' ? 'jpeg' : info.type}`, size: bytes.length, sha256: hash(bytes) };
  }));
  const used = new Set<string>();
  for (const [file, doc] of docs) inSource(file, '链接、段落或图片引用无效', () => walk(doc.nodes, node => {
    if (node.kind !== 'image' && node.kind !== 'link') return;
    const href = node.href ?? '';
    // 链接控制字符不能交给浏览器做宽松归一化。
    // oxlint-disable-next-line no-control-regex
    if (!href || /[\s\u0000-\u001f\\%]/.test(href) || href.startsWith('//')) invalid();
    if (/^https?:\/\//i.test(href)) {
      if (node.kind === 'image') invalid();
      const url = new URL(href); if (url.username || url.password) invalid(); return;
    }
    if (/^[a-z][a-z0-9+.-]*:/i.test(href)) invalid();
    const split = href.split('#'); if (split.length > 2 || href.includes('?')) invalid();
    const [target = '', fragment] = split;
    if (node.kind === 'image') {
      const resolved = posix.normalize(posix.join(posix.dirname(file), target));
      const asset = manifest.assets.find(a => a.file === resolved);
      if (!asset || fragment || target.startsWith('/')) invalid();
      node.href = asset.id; used.add(asset.id); return;
    }
    let destination = target.startsWith('/') ? undefined : docs.get(target ? posix.normalize(posix.join(posix.dirname(file), target)) : file);
    if (!destination && target.startsWith('/')) destination = [...docs.values()].find(item => item.url === target);
    const anchors = new Set<string>();
    if (destination) walk(destination.nodes, n => { if (n.id) anchors.add(n.id); });
    if (target === '/changelog' && fragment) {
      if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(fragment)) invalid();
    } else if (fragment && (!destination || !anchors.has(fragment))) invalid();
    if (!destination && !['/', '/download', '/changelog', '/manual'].includes(target)) invalid();
    // 日志正文段落不暴露第二个 hash：仅文章版本锚点稳定。
    if (destination?.url.includes('#') && fragment) invalid();
    node.href = (destination?.url ?? target) + (fragment ? `#${fragment}` : '');
  }));
  if (used.size !== assets.length) invalid();
  const content: SiteContent = { format: 1, home, manual, assets };
  if (Buffer.byteLength(JSON.stringify(content)) > limits.projection) invalid();
  return content;
}
