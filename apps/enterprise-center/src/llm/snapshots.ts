import { constants } from 'node:fs';
import { chmod, link, lstat, mkdir, open, readdir, realpath, rmdir, unlink } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import { isAbsolute, join, relative, resolve } from 'node:path';

export const snapshotLimit = 8 * 1024 * 1024;
const memoryLimit = 64 * 1024 * 1024;
export type Side = 'request' | 'response';
export type SnapshotState = 'disabled' | 'pending' | 'complete' | 'partial' | 'failed' | 'expired' | 'missing';
export type SnapshotResult = {
  state: SnapshotState;
  path: string | null;
  bytes: number | null;
  sha256: string | null;
  reason: string | null;
};

export const noSnapshot = (state: SnapshotState, reason: string | null = null): SnapshotResult => ({
  state,
  path: null,
  bytes: null,
  sha256: null,
  reason,
});

/** 两侧采集与待写 Buffer 共用一份预算；复制前预留，直到整个调用文件收尾后才释放。 */
export class SnapshotMemory {
  private used = 0;

  reserve(bytes: number): number {
    const allowed = Math.min(bytes, memoryLimit - this.used);
    this.used += allowed;
    return allowed;
  }

  release(bytes: number): void {
    this.used -= bytes;
  }
}

export class Capture {
  private chunks: Buffer[] = [];
  private size = 0;
  reason: string | null = null;

  constructor(private readonly memory: SnapshotMemory) {}

  append(chunk: Buffer): void {
    if (this.reason) return;
    const wanted = Math.min(chunk.length, snapshotLimit - this.size);
    const count = this.memory.reserve(wanted);
    if (count) this.chunks.push(Buffer.from(chunk.subarray(0, count)));
    this.size += count;
    if (count < wanted) this.reason = 'memory_limit';
    else if (wanted < chunk.length) this.reason = 'size_limit';
  }

  interrupt(): void {
    this.reason ??= 'interrupted';
  }

  content(): readonly Buffer[] {
    const original = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
    for (const chunk of this.chunks) original.decode(chunk, { stream: true });
    let length = this.size;
    if (this.reason && length) {
      // 只复制至多四字节检查尾部，发布直接遍历已预留 chunks，避免另聚合一份 8 MiB 正文。
      const tail = Buffer.alloc(Math.min(4, length));
      let cursor = tail.length;
      for (let i = this.chunks.length - 1; i >= 0 && cursor; i--) {
        const chunk = this.chunks[i]!;
        const count = Math.min(cursor, chunk.length);
        chunk.copy(tail, cursor - count, chunk.length - count);
        cursor -= count;
      }
      let start = tail.length - 1;
      while (start >= 0 && (tail[start]! & 0xc0) === 0x80) start--;
      if (start >= 0) {
        const lead = tail[start]!;
        let width = 1;
        if (lead >= 0xc2 && lead <= 0xdf) width = 2;
        else if (lead >= 0xe0 && lead <= 0xef) width = 3;
        else if (lead >= 0xf0 && lead <= 0xf4) width = 4;
        if (width > tail.length - start) length -= tail.length - start;
      }
    }
    const decoder = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true });
    const buffers: Buffer[] = [];
    for (const chunk of this.chunks) {
      const prefix = chunk.subarray(0, Math.min(chunk.length, length));
      decoder.decode(prefix, { stream: true });
      buffers.push(prefix);
      length -= prefix.length;
      if (!length) break;
    }
    decoder.decode();
    return buffers;
  }

  release(): void {
    this.memory.release(this.size);
    this.chunks = [];
    this.size = 0;
  }
}

function absent(error: unknown): boolean {
  return error instanceof Error && 'code' in error && error.code === 'ENOENT';
}

/** 根目录由服务配置拥有；不接受客户端路径。逐层拒绝 symlink，固定文件使用 O_NOFOLLOW。 */
export class SnapshotFiles {
  constructor(
    private readonly root?: string,
    private readonly publicRoot?: string,
  ) {}

  async validate(): Promise<void> {
    if (!this.root || !isAbsolute(this.root)) throw new Error('SNAPSHOT_ROOT_INVALID');
    const root = await realpath(this.root);
    if (root !== resolve(this.root) || !(await lstat(root)).isDirectory()) throw new Error('SNAPSHOT_ROOT_INVALID');
    if (this.publicRoot) {
      const publicRoot = await realpath(this.publicRoot);
      const within = relative(publicRoot, root);
      if (within === '' || (!within.startsWith('..') && !isAbsolute(within))) throw new Error('SNAPSHOT_ROOT_PUBLIC');
    }
    await chmod(root, 0o700);
    const probe = join(root, `.probe-${randomUUID()}`);
    const file = await open(probe, 'wx', 0o600);
    await file.close();
    await unlink(probe);
  }

  private parts(id: string, started: Date): string[] {
    if (
      !this.root ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(id) ||
      !Number.isFinite(started.getTime())
    )
      throw new Error('SNAPSHOT_PATH_INVALID');
    return [...started.toISOString().slice(0, 10).split('-'), id];
  }

  private async directory(id: string, started: Date, create: boolean): Promise<string> {
    let path = this.root!;
    for (const segment of ['', ...this.parts(id, started)]) {
      path = join(path, segment);
      if (create && segment)
        await mkdir(path, { mode: 0o700 }).catch((error) => {
          if (!(error instanceof Error && 'code' in error && error.code === 'EEXIST')) throw error;
        });
      const stat = await lstat(path);
      if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error('SNAPSHOT_PATH_INVALID');
      if (create) await chmod(path, 0o700);
    }
    return path;
  }

  async publish(id: string, started: Date, side: Side, capture: Capture): Promise<SnapshotResult> {
    const chunks = capture.content();
    const digest = createHash('sha256');
    let bytes = 0;
    const directory = await this.directory(id, started, true);
    const part = join(directory, `${side}.part`);
    const file = await open(part, 'wx', 0o600);
    try {
      for (const chunk of chunks) {
        await file.writeFile(chunk);
        digest.update(chunk);
        bytes += chunk.length;
      }
      await file.sync();
    } finally {
      await file.close();
    }
    // link 提供原子且不覆盖的固定名称发布；POSIX rename 会覆盖既有终态文件，故不用它。
    await link(part, join(directory, `${side}.txt`));
    await unlink(part);
    return {
      state: capture.reason ? 'partial' : 'complete',
      path: [...this.parts(id, started), `${side}.txt`].join('/'),
      bytes,
      sha256: digest.digest('hex'),
      reason: capture.reason,
    };
  }

  async read(id: string, started: Date, side: Side): Promise<{ body: Buffer; result: SnapshotResult } | null> {
    try {
      const directory = await this.directory(id, started, false);
      const file = await open(join(directory, `${side}.txt`), constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await file.stat();
        if (!stat.isFile() || stat.size > snapshotLimit) throw new Error('SNAPSHOT_FILE_INVALID');
        const body = await file.readFile();
        return {
          body,
          result: {
            state: 'partial',
            path: [...this.parts(id, started), `${side}.txt`].join('/'),
            bytes: body.length,
            sha256: createHash('sha256').update(body).digest('hex'),
            reason: 'recovered_unknown',
          },
        };
      } finally {
        await file.close();
      }
    } catch (error) {
      if (absent(error)) return null;
      throw error;
    }
  }

  /** 只清理可确认归属的四个固定名称；未知条目/链接使批次停止，不递归扩张删除。 */
  async clean(id: string, started: Date, partsOnly = false): Promise<void> {
    let directory: string;
    try {
      directory = await this.directory(id, started, false);
    } catch (error) {
      if (absent(error)) return;
      throw error;
    }
    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      if (!/^(request|response)\.(txt|part)$/.test(entry.name) || !entry.isFile() || entry.isSymbolicLink())
        throw new Error('SNAPSHOT_UNKNOWN_ENTRY');
    }
    for (const entry of entries)
      if (!partsOnly || entry.name.endsWith('.part')) await unlink(join(directory, entry.name));
    if (!partsOnly) await rmdir(directory);
  }
}
