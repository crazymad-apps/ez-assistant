import { createHash, randomUUID } from 'node:crypto';
import { constants } from 'node:fs';
import { lstat, mkdir, open, link, unlink, realpath } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import type { Readable } from 'node:stream';
import type { ReleaseFile } from './manifest.js';
import { IdentityError } from '../identity/errors.js';

function hasCode(error: unknown, code: string): boolean {
  return error instanceof Error && 'code' in error && error.code === code;
}

/** 只拥有制品文件与传输；发布状态由数据库权威维护，不扫描归档、不运行安装包。 */
export class DesktopReleaseFiles {
  private closing = false;
  private readonly uploads = new Map<AbortController, Promise<unknown>>();
  private readonly downloads = new Set<{ key: string; stream: Readable; done: Promise<void> }>();

  constructor(private readonly root?: string, private readonly publicRoot?: string) {}

  async initialize(): Promise<void> {
    if (!this.root || !isAbsolute(this.root)) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    let cursor: string = sep;
    for (const part of resolve(this.root).split(sep).filter(Boolean)) {
      cursor = join(cursor, part);
      await mkdir(cursor, { mode: 0o700 }).catch(error => { if (!hasCode(error, 'EEXIST')) throw error; });
      const info = await lstat(cursor);
      if (!info.isDirectory() || info.isSymbolicLink()) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    }
    if ((await realpath(this.root)) !== resolve(this.root) || ((await lstat(this.root)).mode & 0o077) !== 0)
      throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    if (this.publicRoot) {
      const publicPath = await realpath(this.publicRoot);
      const inside = (a: string, b: string) => { const r = relative(a, b); return r === '' || (!isAbsolute(r) && r !== '..' && !r.startsWith(`..${sep}`)); };
      if (inside(this.root, publicPath) || inside(publicPath, this.root)) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    }
  }

  async close(): Promise<void> {
    this.closing = true;
    for (const controller of this.uploads.keys()) controller.abort(new IdentityError(503, 'SERVICE_UNAVAILABLE'));
    const streams = [...this.downloads];
    for (const item of streams) item.stream.destroy();
    await Promise.allSettled([...this.uploads.values(), ...streams.map(item => item.done)]);
  }

  cancelDownloads(key: string): void {
    for (const item of this.downloads) if (item.key === key) item.stream.destroy();
  }

  async run<T>(task: (signal: AbortSignal) => Promise<T>, client: AbortSignal): Promise<T> {
    if (!this.root || this.closing) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    if (this.uploads.size >= 2) throw new IdentityError(429, 'RATE_LIMITED');
    const controller = new AbortController();
    const cancel = () => controller.abort(client.reason);
    client.addEventListener('abort', cancel, { once: true });
    if (client.aborted) cancel();
    const timer = setTimeout(() => controller.abort(new IdentityError(408, 'DESKTOP_RELEASE_TIMEOUT')), 30 * 60 * 1000);
    // 微任务中开始，先登记 owner，避免同步失败或关闭期间漏掉在途工作。
    const work = Promise.resolve().then(() => { controller.signal.throwIfAborted(); return task(controller.signal); });
    this.uploads.set(controller, work);
    try { return await work; }
    finally { clearTimeout(timer); client.removeEventListener('abort', cancel); this.uploads.delete(controller); }
  }

  /** SHA/大小与磁盘写入共用上传流；只有完整匹配才发布不可覆盖内容文件。 */
  async receive(stream: Readable, expected: ReleaseFile, signal: AbortSignal): Promise<void> {
    const staged = join(this.root!, `${randomUUID()}.part`);
    const file = await open(staged, 'wx', 0o600);
    const abort = () => stream.destroy(new IdentityError(499, 'DESKTOP_RELEASE_CANCELLED'));
    signal.addEventListener('abort', abort, { once: true });
    const idle = setTimeout(() => stream.destroy(new IdentityError(408, 'DESKTOP_RELEASE_TIMEOUT')), 60000);
    const hash = createHash('sha256');
    let size = 0;
    try {
      signal.throwIfAborted();
      for await (const chunk of stream) {
        signal.throwIfAborted();
        idle.refresh();
        size += chunk.length;
        if (size > expected.size) throw new IdentityError(413, 'DESKTOP_RELEASE_FILE_INVALID');
        hash.update(chunk);
        await file.writeFile(chunk);
      }
      if (!stream.readableEnded || size !== expected.size || hash.digest('hex') !== expected.sha256)
        throw new IdentityError(400, 'DESKTOP_RELEASE_FILE_INVALID');
      await file.sync();
      signal.throwIfAborted();
      try { await link(staged, this.path(expected)); }
      catch (error) {
        if (!hasCode(error, 'EEXIST')) throw error;
        // 重复摘要仅在接纳时复核已有内容，不让被外部改坏的同名文件获得新的有效引用。
        const existing = await this.openFile(expected);
        try {
          const digest = createHash('sha256');
          for await (const chunk of existing.createReadStream({ autoClose: false })) { signal.throwIfAborted(); digest.update(chunk); }
          if (digest.digest('hex') !== expected.sha256) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
        } finally { await existing.close(); }
      }
    } finally {
      clearTimeout(idle); signal.removeEventListener('abort', abort);
      await file.close(); await unlink(staged);
    }
  }

  private path(file: ReleaseFile): string {
    if (!this.root || !/^[a-f0-9]{64}$/.test(file.sha256)) throw new IdentityError(503, 'DESKTOP_RELEASE_STORAGE_UNAVAILABLE');
    return join(this.root, file.sha256);
  }

  private async openFile(expected: ReleaseFile) {
    const file = await open(this.path(expected), constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const info = await file.stat();
      if (!info.isFile() || info.size !== expected.size) throw new IdentityError(503, 'DESKTOP_RELEASE_FILE_UNAVAILABLE');
      return file;
    } catch (error) { await file.close(); throw error; }
  }

  async available(files: ReleaseFile[]): Promise<boolean> {
    try { for (const expected of files) { const file = await this.openFile(expected); await file.close(); } return true; }
    catch { return false; }
  }

  async download(key: string, expected: ReleaseFile): Promise<Readable> {
    if (this.closing) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
    if (this.downloads.size >= 8) throw new IdentityError(429, 'RATE_LIMITED');
    let file;
    try {
      file = await this.openFile(expected);
      if (this.closing || this.downloads.size >= 8) throw new IdentityError(503, 'SERVICE_UNAVAILABLE');
      const stream = file.createReadStream();
      let finish!: () => void;
      const done = new Promise<void>(resolve => { finish = resolve; });
      const item = { key, stream, done };
      this.downloads.add(item);
      stream.once('close', () => { this.downloads.delete(item); finish(); });
      return stream;
    } catch (error) { await file?.close(); throw error; }
  }
}
