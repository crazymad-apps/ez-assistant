import { constants } from 'node:fs';
import { chmod, link, lstat, mkdir, open, readdir, realpath, rmdir, unlink } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import { isAbsolute, join, relative, resolve } from 'node:path';

export const snapshotLimit = 8 * 1024 * 1024;
const memoryLimit = 64 * 1024 * 1024;
export type Side = 'request' | 'response' | 'upstream_failure';
const fileName = (side: Side) => (side === 'upstream_failure' ? 'upstream-failure.bin' : `${side}.txt`);
export type SnapshotState =
  'disabled' | 'pending' | 'complete' | 'partial' | 'failed' | 'expired' | 'missing' | 'not_received';
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
  private reserved = 0;
  reason: string | null = null;

  constructor(
    private readonly memory: SnapshotMemory,
    private readonly limit = snapshotLimit,
  ) {}

  get bytes(): number {
    return this.size;
  }

  append(chunk: Buffer): void {
    if (this.reason) return;
    // 小 delta 复用固定页，避免每字节一个 Buffer 使对象开销突破字节预算。
    let offset = 0;
    while (offset < chunk.length && this.size < this.limit) {
      if (this.size === this.reserved) {
        const count = this.memory.reserve(Math.min(16384, this.limit - this.reserved));
        if (!count) {
          this.reason = 'memory_limit';
          return;
        }
        this.chunks.push(Buffer.allocUnsafe(count));
        this.reserved += count;
      }
      const tail = this.chunks.at(-1)!;
      const used = tail.length - (this.reserved - this.size);
      const count = Math.min(chunk.length - offset, tail.length - used);
      chunk.copy(tail, used, offset, offset + count);
      this.size += count;
      offset += count;
    }
    if (offset < chunk.length) this.reason = 'size_limit';
  }

  interrupt(): void {
    this.reason ??= 'interrupted';
  }

  content(): readonly Buffer[] {
    // 文件是实际采集字节的证据；无效 UTF-8 或截断尾部也必须原样保存。
    if (!this.chunks.length) return [];
    return [
      ...this.chunks.slice(0, -1),
      this.chunks.at(-1)!.subarray(0, this.chunks.at(-1)!.length - (this.reserved - this.size)),
    ];
  }

  release(): void {
    this.memory.release(this.reserved);
    this.reserved = 0;
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
    await link(part, join(directory, fileName(side)));
    await unlink(part);
    return {
      state: capture.reason ? 'partial' : 'complete',
      path: [...this.parts(id, started), fileName(side)].join('/'),
      bytes,
      sha256: digest.digest('hex'),
      reason: capture.reason,
    };
  }

  async read(id: string, started: Date, side: Side): Promise<{ body: Buffer; result: SnapshotResult } | null> {
    try {
      const directory = await this.directory(id, started, false);
      const file = await open(join(directory, fileName(side)), constants.O_RDONLY | constants.O_NOFOLLOW);
      try {
        const stat = await file.stat();
        if (!stat.isFile() || stat.size > snapshotLimit) throw new Error('SNAPSHOT_FILE_INVALID');
        // 不用 readFile：校验后文件被外部扩大时也不得突破单快照读取上限。
        const body = Buffer.alloc(stat.size);
        let offset = 0;
        while (offset < body.length) {
          const { bytesRead } = await file.read(body, offset, body.length - offset, offset);
          if (!bytesRead) throw new Error('SNAPSHOT_FILE_CHANGED');
          offset += bytesRead;
        }
        if ((await file.read(Buffer.alloc(1), 0, 1, offset)).bytesRead) throw new Error('SNAPSHOT_FILE_CHANGED');
        return {
          body,
          result: {
            state: 'partial',
            path: [...this.parts(id, started), fileName(side)].join('/'),
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

  /** 只清理可确认归属的固定快照名称；未知条目/链接使批次停止，不递归扩张删除。 */
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
      if (
        !/^(?:(request|response)\.(txt|part)|upstream_failure\.part|upstream-failure\.bin)$/.test(entry.name) ||
        !entry.isFile() ||
        entry.isSymbolicLink()
      )
        throw new Error('SNAPSHOT_UNKNOWN_ENTRY');
    }
    for (const entry of entries)
      if (!partsOnly || entry.name.endsWith('.part')) await unlink(join(directory, entry.name));
    if (!partsOnly) await rmdir(directory);
  }
}
