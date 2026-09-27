import { createReadStream, createWriteStream } from 'node:fs';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { chmod, mkdtemp, mkdir, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pipeline } from 'node:stream/promises';
import { afterAll, expect, it } from 'vitest';
import { create as createTar } from 'tar';
import { ZipFile } from 'yazl';
import { scanArchive } from '../../dist/plugins/archive.js';
import { PluginFiles } from '../../dist/plugins/files.js';
import { PluginService } from '../../dist/plugins/service.js';
import type { IdentityService } from '../../dist/identity/service.js';

const root = await mkdtemp(join(resolve('../..', '.runtime-test'), 'c01-m0-archive-'));
afterAll(async () => { await rm(root, { recursive: true }); });

function manifest(platforms: { os: string; arch: string }[] = [{ os: 'macos', arch: 'arm64' }]) {
  return {
    format_version: 1, name: 'document-tools', display_name: '文档工具', description: '本地文档',
    version: '1.0.0', platforms, min_host_version: '0.28.0', skills: ['document-review'],
    mcp: ['document-reader'], configuration: [{ name: 'api_token', label: '密钥', required: true, secret: true }],
    external_software: [],
  };
}

function entries(overrides: Record<string, Buffer | string> = {}, platforms?: { os: string; arch: string }[]) {
  const files: Record<string, Buffer | string> = {
    'plugin.json': JSON.stringify(manifest(platforms)),
    'skills/document-review/SKILL.md': '---\nname: document-review\ndescription: Read documents\n---\nRead files.',
    'mcp/document-reader/server.json': JSON.stringify({ mcpServers: { 'document-reader': {
      command: '${plugin_root}/runtimes/macos-arm64/node',
      args: ['${plugin_root}/mcp/document-reader/main.js'],
      env: { API_TOKEN: '${config.api_token}' },
    } } }),
    'mcp/document-reader/main.js': 'process.stdout.write("unused")',
    'runtimes/macos-arm64/node': '#!/bin/sh\nexit 0\n',
  };
  return { ...files, ...overrides };
}

async function zip(files: Record<string, Buffer | string>, filename: string,
  modes: Record<string, number> = {}) {
  const archive = new ZipFile();
  for (const [name, body] of Object.entries(files)) archive.addBuffer(Buffer.from(body), name,
    { mode: modes[name] ?? (name.endsWith('/node') ? 0o100755 : 0o100644), compress: false });
  archive.end();
  await pipeline(archive.outputStream, createWriteStream(filename));
}

async function tar(files: Record<string, Buffer | string>, filename: string) {
  const source = await mkdtemp(join(root, 'source-'));
  for (const [name, body] of Object.entries(files)) {
    const path = join(source, name);
    await mkdir(resolve(path, '..'), { recursive: true });
    await writeFile(path, body);
    if (name.endsWith('/node')) await chmod(path, 0o755);
  }
  await createTar({ file: filename, cwd: source, gzip: true, portable: true },
    ['plugin.json', 'skills', 'mcp', 'runtimes']);
}

it('ZIP 与 TAR.GZ 均读取同一份完整单平台包', async () => {
  const files = entries();
  const zipPath = join(root, 'valid.zip'), tarPath = join(root, 'valid.tar.gz');
  await zip(files, zipPath);
  await tar(files, tarPath);
  expect((await scanArchive(zipPath, 'zip', new AbortController().signal)).name).toBe('document-tools');
  expect((await scanArchive(tarPath, 'tar_gzip', new AbortController().signal)).platforms).toEqual([{ os: 'macos', arch: 'arm64' }]);
  await expect(scanArchive(tarPath, 'zip', new AbortController().signal)).rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('多平台声明缺少对应 MCP 入口时整体拒绝，原包不会发布', async () => {
  const path = join(root, 'incomplete.zip');
  await zip(entries({ 'plugin.json': JSON.stringify(manifest([
    { os: 'macos', arch: 'arm64' }, { os: 'windows', arch: 'x64' },
  ])) }), path);
  await expect(scanArchive(path, 'zip', new AbortController().signal)).rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('多平台完整包同时校验各平台入口和运行时', async () => {
  const path = join(root, 'multi-platform.zip');
  const declared = [{ os: 'macos', arch: 'arm64' }, { os: 'windows', arch: 'x64' }];
  await zip(entries({
    'plugin.json': JSON.stringify(manifest(declared)),
    'mcp/document-reader/server.windows-x64.json': JSON.stringify({ mcpServers: {
      'document-reader': {
        command: '${plugin_root}/runtimes/windows-x64/node.exe',
        args: ['${plugin_root}/mcp/document-reader/main.js'],
        env: { API_TOKEN: '${config.api_token}' },
      },
    } }),
    'runtimes/windows-x64/node.exe': 'sample runtime',
  }), path);
  expect((await scanArchive(path, 'zip', new AbortController().signal)).platforms).toEqual(declared);
});

it('ZIP 实际 CRC 错误与未声明能力均拒绝', async () => {
  const corrupted = join(root, 'corrupted.zip');
  await zip(entries(), corrupted);
  const raw = await readFile(corrupted);
  const position = raw.indexOf(Buffer.from('process.stdout'));
  expect(position).toBeGreaterThan(0);
  raw[position] = raw[position]! ^ 1;
  await writeFile(corrupted, raw);
  await expect(scanArchive(corrupted, 'zip', new AbortController().signal)).rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
  const extra = join(root, 'extra.zip');
  await zip(entries({ 'skills/undeclared/SKILL.md': '---\nname: undeclared\ndescription: extra\n---\n' }), extra);
  await expect(scanArchive(extra, 'zip', new AbortController().signal)).rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('相对符号链接仅能解析到包内文件，不能越过归档根', async () => {
  const valid = join(root, 'linked.zip'), escape = join(root, 'escaping-link.zip');
  const link = 'resources/runtime-link';
  const mode = { [link]: 0o120777 };
  await zip(entries({ [link]: '../runtimes/macos-arm64/node' }), valid, mode);
  expect((await scanArchive(valid, 'zip', new AbortController().signal)).name).toBe('document-tools');
  await zip(entries({ [link]: '../../outside' }), escape, mode);
  await expect(scanArchive(escape, 'zip', new AbortController().signal))
    .rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('TAR 符号链接使用 header 目标，越界目标仍拒绝', async () => {
  const source = await mkdtemp(join(root, 'tar-links-'));
  await mkdir(join(source, 'skills/document-review'), { recursive: true });
  await mkdir(join(source, 'resources'), { recursive: true });
  await writeFile(join(source, 'plugin.json'), JSON.stringify({ ...manifest(), mcp: [] }));
  await writeFile(join(source, 'skills/document-review/SKILL.md'),
    '---\nname: document-review\ndescription: Read documents\n---\nRead files.');
  const { symlink } = await import('node:fs/promises');
  const link = join(source, 'resources/link');
  await symlink('../skills/document-review/SKILL.md', link);
  const valid = join(root, 'valid-link.tar.gz');
  await createTar({ file: valid, cwd: source, gzip: true, portable: true },
    ['plugin.json', 'skills', 'resources']);
  expect((await scanArchive(valid, 'tar_gzip', new AbortController().signal)).name).toBe('document-tools');
  await rm(link);
  await symlink('../../outside', link);
  const escape = join(root, 'escaping-link.tar.gz');
  await createTar({ file: escape, cwd: source, gzip: true, portable: true },
    ['plugin.json', 'skills', 'resources']);
  await expect(scanArchive(escape, 'tar_gzip', new AbortController().signal))
    .rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('Windows 平台拒绝符号链接，大小写冲突对所有平台均拒绝', async () => {
  const collision = join(root, 'collision.zip');
  await zip(entries({ 'resources/Readme': 'one', 'resources/README': 'two' }), collision);
  await expect(scanArchive(collision, 'zip', new AbortController().signal))
    .rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
  const windows = join(root, 'windows-link.zip');
  await zip({
    'plugin.json': JSON.stringify({ ...manifest([{ os: 'windows', arch: 'x64' }]), mcp: [] }),
    'skills/document-review/SKILL.md': '---\nname: document-review\ndescription: Review documents\n---\n',
    'resources/link': '../skills/document-review/SKILL.md',
  }, windows, { 'resources/link': 0o120777 });
  await expect(scanArchive(windows, 'zip', new AbortController().signal))
    .rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
});

it('受保护文件 owner 暂存完整包、按 SHA 发布并复用相同字节', async () => {
  const path = join(root, 'stored.zip');
  await zip(entries(), path);
  const packageRoot = join(root, 'private-packages');
  const files = new PluginFiles(packageRoot);
  await files.initialize();
  try {
    const first = await files.run(signal => files.receive(createReadStream(path), 'zip', signal));
    await files.publish(first);
    expect(await readFile(files.packagePath(first.sha256, 'zip'))).toEqual(await readFile(path));
    expect(await files.check(first.sha256, 'zip', first.size)).toBe(true);
    const second = await files.run(signal => files.receive(createReadStream(path), 'zip', signal));
    expect(second.sha256).toBe(first.sha256);
    await files.publish(second);
    expect(await readdir(join(packageRoot, 'packages'))).toEqual([`${first.sha256}.zip`]);
    expect(await readdir(join(packageRoot, 'staging'))).toEqual([]);
  } finally { await files.close(); }
});

it('下载固定已打开的旧 SHA 文件，下架取消同名在途流且限制并发数', async () => {
  const files = new PluginFiles(join(root, 'downloads'));
  await files.initialize();
  const oldBytes = Buffer.from('old-version-bytes');
  const newBytes = Buffer.from('replacement-bytes');
  const oldSha = createHash('sha256').update(oldBytes).digest('hex');
  const newSha = createHash('sha256').update(newBytes).digest('hex');
  await writeFile(files.packagePath(oldSha, 'zip'), oldBytes);
  await writeFile(files.packagePath(newSha, 'zip'), newBytes);
  try {
    // 两个内容寻址文件并存；当前数据库引用即使切到新 SHA，旧准入仍读原句柄。
    const old = await files.openDownload('document-tools', oldSha, 'zip', oldBytes.length);
    const latest = await files.openDownload('other-tools', newSha, 'zip', newBytes.length);
    const chunks: Buffer[] = [];
    for await (const chunk of old) chunks.push(Buffer.from(chunk));
    expect(Buffer.concat(chunks)).toEqual(oldBytes);
    const closed = once(latest, 'close');
    files.cancelDownloads('other-tools');
    await closed;
    const streams = await Promise.all(Array.from({ length: 8 }, () =>
      files.openDownload('document-tools', oldSha, 'zip', oldBytes.length)));
    await expect(files.openDownload('document-tools', oldSha, 'zip', oldBytes.length))
      .rejects.toMatchObject({ code: 'PACKAGE_BUSY' });
    const closing = streams.map(stream => once(stream, 'close'));
    files.cancelDownloads('document-tools');
    await Promise.all(closing);
  } finally { await files.close(); }
});

it('无效归档只清理本次暂存文件，已发布包保留', async () => {
  const packageRoot = join(root, 'failed-packages');
  const files = new PluginFiles(packageRoot);
  await files.initialize();
  const path = join(root, 'invalid.zip');
  await writeFile(path, Buffer.from('PK\x03\x04invalid'));
  try {
    await expect(files.run(signal => files.receive(createReadStream(path), 'zip', signal)))
      .rejects.toMatchObject({ code: 'PACKAGE_INVALID' });
    expect(await readdir(join(packageRoot, 'staging'))).toEqual([]);
    expect(await readdir(join(packageRoot, 'packages'))).toEqual([]);
  } finally { await files.close(); }
});

it('文件 owner 限制并发并在关闭时取消未完成请求', async () => {
  const files = new PluginFiles(join(root, 'bounded-packages'));
  await files.initialize();
  const pending = () => files.run(signal => new Promise<never>((_resolve, reject) => {
    signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  }));
  const first = pending(), second = pending();
  const firstCheck = expect(first).rejects.toMatchObject({ code: 'PACKAGE_STORAGE_UNAVAILABLE' });
  const secondCheck = expect(second).rejects.toMatchObject({ code: 'PACKAGE_STORAGE_UNAVAILABLE' });
  await expect(pending()).rejects.toMatchObject({ code: 'PACKAGE_BUSY' });
  await files.close();
  await Promise.all([firstCheck, secondCheck]);
  await expect(pending()).rejects.toMatchObject({ code: 'PACKAGE_STORAGE_UNAVAILABLE' });
});

it('提交结果未知时保留已发布 SHA 文件供读取核对，不自动重试', async () => {
  const path = join(root, 'ambiguous-commit.zip');
  await zip(entries(), path);
  const files = new PluginFiles(join(root, 'ambiguous-commit-packages'));
  await files.initialize();
  const identities = { write: async () => { throw new Error('simulated ambiguous commit'); } } as unknown as IdentityService;
  const service = new PluginService(identities, undefined, files);
  try {
    await expect(service.upload(createReadStream(path), 'zip', 'token', 'request', 1))
      .rejects.toMatchObject({ code: 'PACKAGE_RESULT_UNKNOWN' });
    const published = await readdir(join(root, 'ambiguous-commit-packages', 'packages'));
    expect(published).toHaveLength(1);
    expect(await readFile(join(root, 'ambiguous-commit-packages', 'packages', published[0]!)))
      .toEqual(await readFile(path));
    expect(await readdir(join(root, 'ambiguous-commit-packages', 'staging'))).toEqual([]);
  } finally { await files.close(); }
});
