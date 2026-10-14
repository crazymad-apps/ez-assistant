import { constants } from 'node:fs';
import { mkdir, open, realpath, rename, rm } from 'node:fs/promises';
import { join, resolve, sep } from 'node:path';
import { randomUUID } from 'node:crypto';
import { compile, sourceManifest } from './compile.js';
import { ContentSourceError, inSource, invalid, limits } from './validation.js';

/** 本地维护工具只读取显式清单；有界读与无链接检查同时用于首次构建和发布。 */
async function read(file: string, cap: number) {
  if (await realpath(file) !== file) invalid();
  const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    const info = await handle.stat();
    if (!info.isFile() || info.size > cap) invalid();
    const bytes = Buffer.alloc(info.size + 1);
    let length = 0;
    while (length < bytes.length) {
      const result = await handle.read(bytes, length, bytes.length - length, length);
      if (!result.bytesRead) break;
      length += result.bytesRead;
    }
    if (length !== info.size) invalid();
    return bytes.subarray(0, length);
  } finally { await handle.close(); }
}

async function write(file: string, bytes: Buffer) {
  const handle = await open(file, 'wx', 0o644);
  try { await handle.writeFile(bytes); await handle.sync(); }
  finally { await handle.close(); }
}

/** 内容发布不涉及安装包、数据库或服务重启；仅最后一次 rename 改变公开入口。
 * 旧哈希图片保留，已打开的页面可继续使用。失败只删除本次临时 JSON，不删除旧公开内容。
 */
export async function publishDirectory(sourceArg: string, outputArg: string, mode: 'build' | 'publish') {
  const source = resolve(sourceArg), output = resolve(outputArg);
  if (source === output || source.startsWith(output + sep) || output.startsWith(source + sep)) invalid();
  const readSource = async (name: string, cap: number) => {
    try { return await read(join(source, name), cap); }
    catch { throw new ContentSourceError(name, '文件不存在、不可读、超出容量或路径不安全'); }
  };
  const manifestBytes = await readSource('site.json', limits.document);
  const manifest = inSource('site.json', '清单格式、字段或容量无效', () => sourceManifest(manifestBytes)), files = new Map([['site.json', manifestBytes]]);
  let total = manifestBytes.length;
  for (const name of manifest.files.slice(1)) {
    const bytes = await readSource(name, name.startsWith('assets/') ? limits.image : limits.document);
    total += bytes.length;
    if (total > limits.total) invalid();
    files.set(name, bytes);
  }
  const content = compile(files);
  // 先完成全部校验再创建输出。build 禁止覆盖；publish 必须显式给出专用公开目录。
  await mkdir(output, { recursive: mode === 'publish', mode: 0o755 });
  if (await realpath(output) !== output) invalid();
  const assets = join(output, 'assets');
  await mkdir(assets, { recursive: true, mode: 0o755 });
  if (await realpath(assets) !== assets) invalid();
  const pending = join(output, `.content-${randomUUID()}.json`);
  try {
    for (const asset of content.assets) {
      const bytes = files.get(manifest.assets.find(item => item.id === asset.id)!.file)!;
      const file = join(assets, `${asset.sha256}.${asset.type.slice(6)}`);
      const pendingImage = join(assets, `.image-${randomUUID()}`);
      try {
        // 同摘要只能对应同一字节；先写临时文件再替换，也允许重新发布修复损坏的图片。
        await write(pendingImage, bytes);
        await rename(pendingImage, file);
      } finally { await rm(pendingImage, { force: true }); }
    }
    await write(pending, Buffer.from(JSON.stringify(content)));
    // 同盘原子切换，不直接写正在被访客读取的 content.json。
    await rename(pending, join(output, 'content.json'));
  } finally { await rm(pending, { force: true }); }
  return content;
}
