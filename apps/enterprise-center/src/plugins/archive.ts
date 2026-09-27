import { createReadStream } from 'node:fs';
import { open } from 'node:fs/promises';
import { posix } from 'node:path';
import { createGunzip, crc32 } from 'node:zlib';
import { Parser } from 'tar';
import type { ReadEntry } from 'tar';
import { openPromise } from 'yauzl';
import type { Entry, ZipFile } from 'yauzl';
import { invalidPackage, oversizedPackage, PluginError } from './errors.js';
import { parseManifest, validateMcp, validateSkill } from './manifest.js';
import type { PluginManifest } from './manifest.js';

const totalLimit = 8 * 1024 * 1024 * 1024;
const fileLimit = 2 * 1024 * 1024 * 1024;
const entryLimit = 100000;
const descriptorLimit = 1024 * 1024;
const allowedRoot = new Set(['skills', 'mcp', 'runtimes', 'resources', 'licenses']);

type Item = { type: 'file' | 'directory' | 'link'; mode: number; link?: string; chunks?: Buffer[]; size: number };

/** 除目录尾斜杠外，路径必须已是唯一的包内规范形式；不替上传者修正逃逸或冲突路径。 */
function cleanPath(path: string, directory = false): string {
  if (directory && path.endsWith('/')) path = path.slice(0, -1);
  if (!path || Buffer.byteLength(path) > 1024 || path !== path.normalize('NFC') ||
    path.startsWith('/') || path.startsWith('//') || /^[A-Za-z]:/.test(path) ||
    path.includes('\\') || path.includes('\0') || path.includes('\ufffd') ||
    path.split('/').some(part => !part || part === '.' || part === '..')) throw invalidPackage();
  if (path !== 'plugin.json' && !allowedRoot.has(path.split('/')[0]!)) throw invalidPackage();
  return path;
}

function descriptor(path: string): boolean {
  return path === 'plugin.json' || /^skills\/[^/]+\/SKILL\.md$/.test(path) ||
    /^mcp\/[^/]+\/server(?:\.(?:macos|windows|linux)-(?:x64|arm64))?\.json$/.test(path);
}

/** 只缓存受限的清单/入口描述文件；其他条目逐块计数校验，不把完整解压内容放进内存。 */
class Index {
  readonly entries = new Map<string, Item>();
  private total = 0;

  add(path: string, type: Item['type'], mode: number, expected: number): { append: (chunk: Buffer) => void; end: () => void } {
    path = cleanPath(path, type === 'directory');
    if (this.entries.has(path) || this.entries.size >= entryLimit ||
      !Number.isSafeInteger(expected) || expected < 0 || expected > fileLimit ||
      (mode & 0o6000) !== 0 || (type === 'directory' && expected !== 0)) throw invalidPackage();
    const item: Item = { type, mode, size: 0 };
    this.entries.set(path, item);
    if (type === 'file' && descriptor(path)) item.chunks = [];
    if (type === 'link') item.chunks = [];
    return {
      append: (chunk: Buffer) => {
        item.size += chunk.length;
        this.total += chunk.length;
        if (item.size > fileLimit || this.total > totalLimit ||
          (item.chunks && item.size > (type === 'link' ? 1024 : descriptorLimit))) throw oversizedPackage();
        if (item.chunks) item.chunks.push(Buffer.from(chunk));
      },
      end: () => {
        if (item.size !== expected) throw invalidPackage();
        // TAR 的目标在 header.linkpath，ZIP 的目标才来自条目正文。
        if (type === 'link' && item.chunks) {
          try { item.link = new TextDecoder('utf-8', { fatal: true }).decode(Buffer.concat(item.chunks!)); }
          catch { throw invalidPackage(); }
          item.chunks = undefined;
        }
      },
    };
  }

  setTarLink(path: string, link: string): void {
    const item = this.entries.get(path)!;
    item.link = link;
    item.chunks = undefined;
  }

  /** 符号链接只做包内词法解析与循环检查，中心不执行或解包到文件系统。 */
  private resolved(path: string, seen = new Set<string>()): Item | undefined {
    if (seen.has(path)) throw invalidPackage();
    seen.add(path);
    const item = this.entries.get(path);
    if (!item || item.type !== 'link') return item;
    const target = item.link;
    if (!target || Buffer.byteLength(target) > 1024 || target.includes('\ufffd') ||
      target.startsWith('/') || /^[A-Za-z]:/.test(target) || target.includes('\\') || target.includes('\0'))
      throw invalidPackage();
    const resolved = posix.normalize(posix.join(posix.dirname(path), target));
    if (resolved === '..' || resolved.startsWith('../') || resolved.startsWith('/')) throw invalidPackage();
    return this.resolved(resolved, seen);
  }

  private bytes(path: string): Buffer {
    const item = this.entries.get(path);
    if (item?.type !== 'file' || !item.chunks) throw invalidPackage();
    return Buffer.concat(item.chunks);
  }

  /** 全部条目读完后再按清单核对所有声明平台，任一平台缺入口即拒绝整个包。 */
  finish(): PluginManifest {
    for (const [path, item] of this.entries) {
      for (const parent of parents(path)) {
        const found = this.entries.get(parent);
        if (found && found.type !== 'directory') throw invalidPackage();
      }
      if (item.type === 'link' && !this.resolved(path)) throw invalidPackage();
    }
    const manifest = parseManifest(this.bytes('plugin.json'));
    const platformNames = new Set(manifest.platforms.map(item => `${item.os}-${item.arch}`));
    const windows = manifest.platforms.some(item => item.os === 'windows');
    for (const [path, item] of this.entries) {
      if (windows) {
        if (item.type === 'link' || path.split('/').some(part =>
          /[<>:"|?*]/.test(part) || /[. ]$/.test(part) ||
          /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) throw invalidPackage();
      }
      const root = path.split('/');
      if (root[0] === 'skills' && root.length > 1 && !manifest.skills.includes(root[1]!)) throw invalidPackage();
      if (root[0] === 'mcp' && root.length > 1 && !manifest.mcp.includes(root[1]!)) throw invalidPackage();
      if (root[0] === 'runtimes' && root.length > 1 && !platformNames.has(root[1]!)) throw invalidPackage();
      if (item.type === 'link' && item.link && !this.resolved(path)) throw invalidPackage();
    }
    // 大小写折叠统一拒绝冲突，避免同一个包在大小写敏感与不敏感平台安装出不同结果。
    const folded = new Set<string>();
    for (const path of this.entries.keys()) {
      const key = path.normalize('NFC').toLowerCase();
      if (folded.has(key)) throw invalidPackage();
      folded.add(key);
    }
    for (const name of manifest.skills) validateSkill(this.bytes(`skills/${name}/SKILL.md`), name);
    for (const name of manifest.mcp) {
      const common = `mcp/${name}/server.json`;
      if (this.entries.has(common)) validateMcp(this.bytes(common), name, manifest.platforms[0]!, manifest,
        target => { const item = this.resolved(target); return item?.type === 'file' ? item.mode : undefined; },
        target => target === '' || this.resolved(target)?.type === 'directory' ||
          [...this.entries.keys()].some(entry => entry.startsWith(`${target}/`)));
      for (const path of this.entries.keys()) {
        const prefix = `mcp/${name}/server.`;
        if (path.startsWith(prefix) && path.endsWith('.json')) {
          const variant = path.slice(prefix.length, -5);
          if (/^(?:macos|windows|linux)-(?:x64|arm64)$/.test(variant) && !platformNames.has(variant))
            throw invalidPackage();
        }
      }
      for (const platform of manifest.platforms) {
        const path = `mcp/${name}/server.${platform.os}-${platform.arch}.json`;
        const selected = this.entries.has(path) ? path : `mcp/${name}/server.json`;
        validateMcp(this.bytes(selected), name, platform, manifest,
          target => { const item = this.resolved(target); return item?.type === 'file' ? item.mode : undefined; },
          target => target === '' || this.resolved(target)?.type === 'directory' ||
            [...this.entries.keys()].some(entry => entry.startsWith(`${target}/`)));
      }
    }
    return manifest;
  }
}

function parents(path: string): string[] {
  const result: string[] = [];
  let cursor = posix.dirname(path);
  while (cursor !== '.') { result.push(cursor); cursor = posix.dirname(cursor); }
  return result;
}

async function scanTar(path: string, index: Index, signal: AbortSignal): Promise<void> {
  const parser = new Parser({ strict: true, maxMetaEntrySize: descriptorLimit, maxDecompressionRatio: 100000 });
  let ended = false;
  let failure: unknown;
  parser.on('end', () => { ended = true; });
  parser.on('error', error => { failure = error; });
  parser.on('entry', (entry: ReadEntry) => {
    if (failure) return;
    try {
      const type = entry.type === 'File' || entry.type === 'OldFile' ? 'file' :
        entry.type === 'Directory' ? 'directory' : entry.type === 'SymbolicLink' ? 'link' : undefined;
      if (!type || entry.invalid || entry.unsupported) throw invalidPackage();
      // PAX 只接受归档定位所需元数据，不让额外键改变安装端对条目的解释。
      for (const pax of [entry.extended, entry.globalExtended]) {
        if (pax && Object.keys(pax).some(key =>
          !['global', 'path', 'linkpath', 'mtime', 'atime', 'ctime', 'size'].includes(key))) throw invalidPackage();
      }
      const name = cleanPath(entry.path, type === 'directory');
      const collector = index.add(name, type, entry.mode ?? 0o644, entry.size);
      if (type === 'link') {
        index.setTarLink(name, entry.linkpath ?? '');
      }
      // Parser 的 entry 回调异步交付正文；将校验错误传回外层扫描循环统一终止。
      entry.on('data', (chunk: Buffer) => { try { collector.append(chunk); } catch (error) { failure = error; } });
      entry.on('end', () => { try { collector.end(); } catch (error) { failure = error; } });
      entry.resume();
    } catch (error) { failure = error; entry.resume(); }
  });
  const stream = createReadStream(path).pipe(createGunzip());
  let total = 0;
  try {
    for await (const chunk of stream) {
      if (signal.aborted) throw signal.reason;
      total += chunk.length;
      if (total > totalLimit + entryLimit * 512) throw oversizedPackage();
      parser.write(chunk);
      if (failure) throw failure;
    }
    parser.end();
    if (failure || !ended) throw failure ?? invalidPackage();
  } finally { stream.destroy(); }
}

function nextEntry(zip: ZipFile): Promise<Entry | null> {
  return new Promise((resolve, reject) => {
    const cleanup = () => { zip.off('entry', onEntry); zip.off('end', onEnd); zip.off('error', onError); };
    const onEntry = (entry: Entry) => { cleanup(); resolve(entry); };
    const onEnd = () => { cleanup(); resolve(null); };
    const onError = (error: Error) => { cleanup(); reject(error); };
    zip.once('entry', onEntry);
    zip.once('end', onEnd);
    zip.once('error', onError);
    zip.readEntry();
  });
}

function entryStream(zip: ZipFile, entry: Entry): Promise<NodeJS.ReadableStream> {
  return new Promise((resolve, reject) => zip.openReadStream(entry, (error, stream) =>
    error ? reject(error) : resolve(stream)));
}

async function scanZip(path: string, index: Index, signal: AbortSignal): Promise<void> {
  const zip = await openPromise(path, { lazyEntries: true, validateEntrySizes: true, strictFileNames: true });
  try {
    if (zip.entryCount > entryLimit) throw oversizedPackage();
    let count = 0;
    while (true) {
      if (signal.aborted) throw signal.reason;
      const entry = await nextEntry(zip);
      if (!entry) break;
      count++;
      // 非 ASCII 文件名必须有 UTF-8 标志或 CRC 匹配的 Unicode 路径字段，拒绝依赖本机编码猜测。
      const unicodeName = entry.extraFields.some(field => field.id === 0x7075 && field.data.length >= 5 &&
        field.data[0] === 1 && field.data.readUInt32LE(1) === crc32(entry.fileNameRaw));
      if (entry.isEncrypted() || ![0, 8].includes(entry.compressionMethod) ||
        (entry.generalPurposeBitFlag & 0x40) !== 0 ||
        (!(entry.generalPurposeBitFlag & 0x800) && !unicodeName &&
          entry.fileNameRaw.some(byte => byte > 0x7f))) throw invalidPackage();
      const mode = entry.externalFileAttributes >>> 16;
      const fileType = mode & 0o170000;
      const directory = entry.fileName.endsWith('/');
      const type = fileType === 0o120000 ? 'link' : directory || fileType === 0o040000 ? 'directory' :
        fileType === 0 || fileType === 0o100000 ? 'file' : undefined;
      if (!type || (type === 'directory') !== directory) throw invalidPackage();
      const collector = index.add(entry.fileName, type, mode, entry.uncompressedSize);
      if (type !== 'directory') {
        let crc = 0;
        for await (const chunk of await entryStream(zip, entry)) {
          if (signal.aborted) throw signal.reason;
          const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
          collector.append(bytes);
          crc = crc32(bytes, crc);
        }
        // central directory 声明不足以证明文件完整，按实际解压字节再核对 CRC。
        if (crc !== entry.crc32) throw invalidPackage();
      }
      collector.end();
    }
    if (count !== zip.entryCount) throw invalidPackage();
  } finally { zip.close(); }
}

/** 先核对声明格式与魔数，再流式扫描；存储故障与不可信包错误保持不同对外语义。 */
export async function scanArchive(path: string, declared: 'zip' | 'tar_gzip', signal: AbortSignal): Promise<PluginManifest> {
  const file = await open(path, 'r');
  let header: Buffer;
  try {
    header = Buffer.alloc(4);
    await file.read(header, 0, 4, 0);
  } finally { await file.close(); }
  if (declared === 'zip' ? !header.equals(Buffer.from([0x50, 0x4b, 0x03, 0x04])) :
    header[0] !== 0x1f || header[1] !== 0x8b) throw invalidPackage();
  const index = new Index();
  try {
    if (declared === 'zip') await scanZip(path, index, signal);
    else await scanTar(path, index, signal);
    return index.finish();
  } catch (error) {
    if (error instanceof PluginError) throw error;
    if (signal.aborted) throw signal.reason;
    if (error instanceof Error && 'code' in error &&
        ['EIO', 'ENOSPC', 'EACCES', 'EPERM', 'ENOENT', 'EMFILE'].includes(String(error.code)))
      throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    throw invalidPackage();
  }
}
