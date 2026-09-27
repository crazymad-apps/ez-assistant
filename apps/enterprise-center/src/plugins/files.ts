import { createHash, randomUUID } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { constants } from 'node:fs';
import { lstat, link, mkdir, open, readdir, realpath, unlink } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import type { Readable } from 'node:stream';
import { invalidPackage, oversizedPackage, PluginError } from './errors.js';
import { scanArchive } from './archive.js';
import type { PluginManifest } from './manifest.js';

const compressedLimit = 2 * 1024 * 1024 * 1024;
export type PreparedPackage = {
  manifest: PluginManifest;
  format: 'zip' | 'tar_gzip';
  sha256: string;
  size: number;
  staged: string;
};

function hasCode(error: unknown, code: string): boolean {
  return error instanceof Error && 'code' in error && error.code === code;
}

/** 文件 owner 只管理压缩包字节和请求生命周期；数据库当前引用由服务短事务持有。 */
export class PluginFiles {
  private readonly active = new Set<AbortController>();
  private readonly pending = new Set<Promise<unknown>>();
  private readonly downloads = new Set<{ name: string; stream: Readable; done: Promise<void> }>();
  private closing = false;

  constructor(private readonly root?: string, private readonly publicRoot?: string) {}

  private get staging(): string { return join(this.root!, 'staging'); }
  private get packages(): string { return join(this.root!, 'packages'); }

  /** 启动时先确认私有目录不经符号链接落入公开资源，再清理上次中断留下的暂存文件。 */
  async initialize(): Promise<void> {
    if (!this.root || !isAbsolute(this.root)) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    let cursor = resolve(this.root).startsWith(sep) ? sep : '';
    for (const part of resolve(this.root).split(sep).filter(Boolean)) {
      cursor = join(cursor, part);
      try { await mkdir(cursor, { mode: 0o700 }); }
      catch (error) { if (!hasCode(error, 'EEXIST')) throw error; }
      const item = await lstat(cursor);
      if (!item.isDirectory() || item.isSymbolicLink())
        throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    }
    for (const child of [this.staging, this.packages]) {
      try { await mkdir(child, { mode: 0o700 }); }
      catch (error) { if (!hasCode(error, 'EEXIST')) throw error; }
      const item = await lstat(child);
      if (!item.isDirectory() || item.isSymbolicLink() || (item.mode & 0o077) !== 0)
        throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    }
    if ((await realpath(this.root)) !== resolve(this.root) || ((await lstat(this.root)).mode & 0o077) !== 0)
      throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    if (this.publicRoot) {
      const publicPath = await realpath(this.publicRoot);
      const inside = (a: string, b: string) => { const r = relative(a, b); return r === '' || (r !== '..' && !r.startsWith(`..${sep}`) && !isAbsolute(r)); };
      if (inside(publicPath, this.root) || inside(this.root, publicPath)) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    }
    // staging 只归本 owner 管理；出现非本程序临时文件时拒绝启动，避免误删其他数据。
    for (const item of await readdir(this.staging, { withFileTypes: true })) {
      if (!/^[0-9a-f-]{36}\.part$/.test(item.name) || !item.isFile() || item.isSymbolicLink())
        throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
      await unlink(join(this.staging, item.name));
    }
    const probe = join(this.staging, `${randomUUID()}.part`);
    const file = await open(probe, 'wx', 0o600);
    await file.close();
    await unlink(probe);
  }

  /** 关闭准入并等待本 owner 的接收、扫描和提交结束。 */
  async close(): Promise<void> {
    this.closing = true;
    for (const controller of this.active) controller.abort(new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE'));
    const downloads = [...this.downloads];
    for (const item of downloads) item.stream.destroy();
    await Promise.allSettled(this.pending);
    await Promise.allSettled(downloads.map(item => item.done));
  }

  /** 只取消仍在传输的该插件下载；已经结束的字节不能撤回，也不删除旧 SHA 文件。 */
  cancelDownloads(name: string): void {
    for (const item of this.downloads) if (item.name === name) item.stream.destroy();
  }

  /** 并发名额覆盖接收、扫描和短事务；关闭或客户端取消通过同一信号停止未提交工作。 */
  async run<T>(task: (signal: AbortSignal) => Promise<T>, clientSignal?: AbortSignal): Promise<T> {
    if (this.closing) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    if (this.active.size >= 2) throw new PluginError(429, 'PACKAGE_BUSY');
    const controller = new AbortController();
    const clientAbort = () => controller.abort(clientSignal?.reason ?? new PluginError(499, 'PACKAGE_CANCELLED'));
    clientSignal?.addEventListener('abort', clientAbort, { once: true });
    if (clientSignal?.aborted) clientAbort();
    const deadline = setTimeout(() => controller.abort(new PluginError(408, 'PACKAGE_TIMEOUT')), 35 * 60 * 1000);
    this.active.add(controller);
    const work = task(controller.signal);
    this.pending.add(work);
    try { return await work; }
    finally {
      clearTimeout(deadline);
      clientSignal?.removeEventListener('abort', clientAbort);
      this.active.delete(controller);
      this.pending.delete(work);
    }
  }

  /** 上传流只写入私有暂存文件并计算原包 SHA；收完、落盘后才扫描，不占用数据库事务。 */
  async receive(body: Readable, format: PreparedPackage['format'], signal: AbortSignal): Promise<PreparedPackage> {
    if (!this.root) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    const staged = join(this.staging, `${randomUUID()}.part`);
    const file = await open(staged, 'wx', 0o600);
    const abort = () => body.destroy(signal.reason);
    signal.addEventListener('abort', abort, { once: true });
    const receivedBy = setTimeout(() => body.destroy(new PluginError(408, 'PACKAGE_TIMEOUT')), 30 * 60 * 1000);
    let idle = setTimeout(() => body.destroy(new PluginError(408, 'PACKAGE_TIMEOUT')), 60 * 1000);
    let size = 0;
    const hash = createHash('sha256');
    try {
      for await (const chunk of body) {
        clearTimeout(idle);
        idle = setTimeout(() => body.destroy(new PluginError(408, 'PACKAGE_TIMEOUT')), 60 * 1000);
        if (signal.aborted) throw signal.reason;
        size += chunk.length;
        if (size > compressedLimit) throw oversizedPackage();
        hash.update(chunk);
        await file.writeFile(chunk);
      }
      // 半包断流不能作为有效归档，扫描超时也独立于接收超时计算。
      if (!body.readableEnded || !size) throw invalidPackage();
      clearTimeout(receivedBy);
      clearTimeout(idle);
      await file.sync();
      await file.close();
      const scan = new AbortController();
      const cancelScan = () => scan.abort(signal.reason);
      signal.addEventListener('abort', cancelScan, { once: true });
      if (signal.aborted) cancelScan();
      const scanDeadline = setTimeout(() => scan.abort(new PluginError(408, 'PACKAGE_TIMEOUT')), 5 * 60 * 1000);
      let manifest: PluginManifest;
      try { manifest = await scanArchive(staged, format, scan.signal); }
      finally { clearTimeout(scanDeadline); signal.removeEventListener('abort', cancelScan); }
      return { manifest, format, sha256: hash.digest('hex'), size, staged };
    } catch (error) {
      await file.close().catch(() => undefined);
      await unlink(staged).catch(() => undefined);
      if (error instanceof PluginError) throw error;
      if (signal.aborted) throw signal.reason;
      throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    } finally { clearTimeout(receivedBy); clearTimeout(idle); signal.removeEventListener('abort', abort); }
  }

  /** 硬链接以不覆盖方式发布原始字节；同 SHA 已存在时核对完整散列后才复用。 */
  async publish(packageData: PreparedPackage): Promise<void> {
    const destination = this.packagePath(packageData.sha256, packageData.format);
    try {
      await link(packageData.staged, destination);
      const directory = await open(this.packages, 'r');
      try { await directory.sync(); } finally { await directory.close(); }
    } catch (error) {
      if (!hasCode(error, 'EEXIST')) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
      const existing = await lstat(destination);
      if (!existing.isFile() || existing.isSymbolicLink() || existing.size !== packageData.size)
        throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
      const digest = createHash('sha256');
      for await (const chunk of createReadStream(destination)) digest.update(chunk);
      if (digest.digest('hex') !== packageData.sha256)
        throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    } finally { await unlink(packageData.staged).catch(() => undefined); }
  }

  packagePath(sha256: string, format: PreparedPackage['format']): string {
    if (!this.root || !/^[0-9a-f]{64}$/.test(sha256)) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    return join(this.packages, `${sha256}.${format === 'zip' ? 'zip' : 'tar.gz'}`);
  }

  /** 常规读取只核对类型和长度；完整散列留给备份恢复及实际传输校验。 */
  async check(sha256: string, format: PreparedPackage['format'], size: number): Promise<boolean> {
    try {
      const file = await open(this.packagePath(sha256, format), constants.O_RDONLY | constants.O_NOFOLLOW);
      try { const info = await file.stat(); return info.isFile() && info.size === size; }
      finally { await file.close(); }
    } catch { return false; }
  }

  /** 准入时固定打开的文件句柄，替换当前 SHA 后仍可完成先前已准入的下载。 */
  async openDownload(name: string, sha256: string, format: PreparedPackage['format'], size: number): Promise<Readable> {
    if (this.closing) throw new PluginError(503, 'PACKAGE_STORAGE_UNAVAILABLE');
    if (this.downloads.size >= 8) throw new PluginError(429, 'PACKAGE_BUSY');
    let file;
    try {
      file = await open(this.packagePath(sha256, format), constants.O_RDONLY | constants.O_NOFOLLOW);
      const info = await file.stat();
      if (!info.isFile() || info.size !== size) throw new PluginError(503, 'PACKAGE_UNAVAILABLE');
      const stream = file.createReadStream();
      stream.pause();
      let finished!: () => void;
      const done = new Promise<void>(resolve => { finished = resolve; });
      const item = { name, stream, done };
      this.downloads.add(item);
      stream.once('close', () => { this.downloads.delete(item); finished(); });
      return stream;
    } catch (error) {
      await file?.close().catch(() => undefined);
      if (error instanceof PluginError) throw error;
      throw new PluginError(503, 'PACKAGE_UNAVAILABLE');
    }
  }
}
