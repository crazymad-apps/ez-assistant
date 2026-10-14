import { expect, it } from 'vitest';
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink } from 'node:fs/promises';
import { resolve, join, dirname } from 'node:path';
import Fastify from 'fastify';
import { compile } from '../../dist/site-content/compile.js';
import { publishDirectory } from '../../dist/site-content/directory.js';
import { registerSiteContent } from '../../dist/site-content/static.js';
import { releaseNotes } from '../../dist/site-content/markdown.js';
import { siteFixture } from '../support/site-content.js';

it('受限编译保留正文和稳定链接，拒绝脚本、HTML、外链图片、悬空引用及异常元信息', () => {
  const result = compile(siteFixture());
  expect(result.manual[0]!.articles[0]!.body[0]!.id).toBe('install');
  expect(JSON.stringify(result.home)).toContain('/manual/installation#install');
  expect(result.assets[0]!.type).toBe('image/png');
  for (const body of ['<script>alert(1)</script>', '[x](javascript:alert(1))', '![x](https://example.test/x.png)', '[x](../manual/missing.md)', '[x](../manual/installation.md#missing)', '[x](data:text/html,x)', '[x](//example.test)', '[x](/admin/)']) {
    const files = siteFixture(); files.set('home/hero.md', Buffer.from(body));
    expect(() => compile(files), body).toThrow();
  }
  for (const [name, bytes] of [
    ['extra.txt', Buffer.from('unlisted')],
    ['assets/sample.png', Buffer.from('<svg/>')],
    ['manual/installation.md', Buffer.from('---\nid: other\n---\n# X')],
    ['home/hero.md', Buffer.from('x'.repeat(128 * 1024 + 1))],
    ['home/hero.md', Buffer.from([0xff])],
  ] as [string, Buffer][]) { const files = siteFixture(); files.set(name, bytes); expect(() => compile(files)).toThrow(); }
  const badDate = siteFixture(); badDate.set('manual/installation.md', Buffer.from(badDate.get('manual/installation.md')!.toString().replace('2026-10-13', '2026-02-30'))); expect(() => compile(badDate)).toThrow();
});

it('版本说明安全降级：支持 Markdown，危险链接和远程图片不执行，旧格式不阻断公开列表', () => {
  expect(releaseNotes('**改进**')[0]!.children!.some(node => node.kind === 'strong')).toBe(true);
  for (const notes of ['<script>alert(1)</script>', '[x](javascript:alert(1))', '![x](https://example.test/a.png)', '[x](//example.test)', '[x](data:text/html,x)']) {
    const nodes = releaseNotes(notes);
    expect(nodes.flatMap(node => node.children ?? []).some(node => node.kind === 'image' || node.kind === 'link')).toBe(false);
  }
  expect(releaseNotes('## 同名\n\n## 同名')[0]!.children![0]!.text).toContain('## 同名');
});

it('静态目录独立更新：非法修改保留旧入口，旧图可读，缺失／越界／软链不公开', async () => {
  const base = resolve('../../.runtime-test'); await mkdir(base, { recursive: true });
  const root = await mkdtemp(join(base, 'v0.31.0-f01-static-'));
  const source = join(root, 'source'), output = join(root, 'public');
  const server = Fastify(); registerSiteContent(server, output);
  try {
    for (const [name, bytes] of siteFixture()) { await mkdir(dirname(join(source, name)), { recursive: true }); await writeFile(join(source, name), bytes); }
    expect((await server.inject('/site-content/content.json')).statusCode).toBe(404);
    const content = await publishDirectory(source, output, 'build');
    const first = await readFile(join(output, 'content.json'));
    expect(content).not.toHaveProperty('release_version'); expect(content).not.toHaveProperty('changelog');
    const asset = content.assets[0]!, image = `/site-content/assets/${asset.sha256}.png`;
    expect((await server.inject(image)).rawPayload).toEqual(siteFixture().get('assets/sample.png'));
    expect((await server.inject('/site-content/content.json')).headers['cache-control']).toBe('no-cache');
    await writeFile(join(source, 'home/hero.md'), '<script>bad</script>');
    await expect(publishDirectory(source, output, 'publish')).rejects.toThrow();
    expect(await readFile(join(output, 'content.json'))).toEqual(first);
    await writeFile(join(source, 'home/hero.md'), siteFixture('0.31.0', '新手册').get('home/hero.md')!);
    await publishDirectory(source, output, 'publish');
    expect((await server.inject('/site-content/content.json')).body).toContain('新手册');
    expect((await server.inject(image)).statusCode).toBe(200);
    await writeFile(join(output, 'assets', `${asset.sha256}.png`), 'damaged');
    await publishDirectory(source, output, 'publish');
    expect((await server.inject(image)).rawPayload).toEqual(siteFixture().get('assets/sample.png'));
    await expect(publishDirectory(source, output, 'build')).rejects.toThrow();
    for (const path of ['/site-content/.content-secret.json', '/site-content/site.json', '/site-content/assets/../../secret', '/site-content/assets/missing.png']) expect((await server.inject(path)).statusCode).toBe(404);
    await writeFile(join(root, 'private'), 'secret');
    await rm(join(output, 'content.json')); await symlink(join(root, 'private'), join(output, 'content.json'));
    expect((await server.inject('/site-content/content.json')).statusCode).toBe(404);
    await rm(join(source, 'home/hero.md')); await symlink(join(root, 'private'), join(source, 'home/hero.md'));
    await expect(publishDirectory(source, output, 'publish')).rejects.toThrow();
  } finally { await server.close(); await rm(root, { recursive: true, force: true }); }
});

it('工作台场景只承载受限静态文字，拒绝重复身份、未知图形与跨区配置', () => {
  const files = siteFixture();
  const manifest = JSON.parse(files.get('site.json')!.toString());
  const scene = { id: 'example', label: '场景', plugin: '插件', prompt: '输入', intro: '过程', title: '结果', body: '说明', file: 'output.dxf', reply: '答复', steps: ['核对'], kind: 'drawing' };
  manifest.home[0].kind = 'workspace'; manifest.home[0].scenarios = [scene];
  const compiled = () => { files.set('site.json', Buffer.from(JSON.stringify(manifest))); return compile(files); };
  expect(compiled().home[0]!.scenarios![0]).toEqual(scene);
  manifest.home[0].scenarios = [scene, scene]; expect(compiled).toThrow();
  manifest.home[0].scenarios = [{ ...scene, kind: 'script' }]; expect(compiled).toThrow();
  manifest.home[0].scenarios = [scene]; manifest.home[0].kind = 'hero'; expect(compiled).toThrow();
});

it('内容校验仅定位相对源文件与固定原因，不输出原正文', () => {
  const files = siteFixture();
  files.set('home/hero.md', Buffer.from('<script>private-fixture-value</script>'));
  expect(() => compile(files)).toThrow('home/hero.md：Markdown 格式无效或含不支持内容');
  try { compile(files); } catch (error) { expect(String(error)).not.toContain('private-fixture-value'); }
  files.set('home/hero.md', Buffer.from('[missing](../manual/absent.md)'));
  expect(() => compile(files)).toThrow('home/hero.md：链接、段落或图片引用无效');
});
