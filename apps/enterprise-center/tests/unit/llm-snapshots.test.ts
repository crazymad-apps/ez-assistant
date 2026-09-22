import { mkdtemp, mkdir, readFile, lstat, symlink } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { expect, it } from 'vitest';
import { SnapshotFiles, SnapshotMemory, Capture, snapshotLimit } from '../../dist/llm/snapshots.js';

it('UTF-8 截断只裁尾字节，完整或中间损坏不替换为伪完整文本，采集预算可释放', () => {
  const memory = new SnapshotMemory();
  const captures = Array.from({ length: 8 }, () => new Capture(memory));
  for (const capture of captures) capture.append(Buffer.alloc(snapshotLimit, 65));
  const limited = new Capture(memory);
  limited.append(Buffer.from('test'));
  expect(limited.reason).toBe('memory_limit');
  for (const capture of captures) capture.release();
  const partial = new Capture(memory);
  partial.append(Buffer.from([0x61, 0xe4, 0xb8]));
  expect(() => partial.content()).toThrow();
  partial.interrupt();
  expect(Buffer.concat(partial.content()).toString()).toBe('a');
  partial.release();
  const invalid = new Capture(memory);
  invalid.append(Buffer.from([0xff, 0x61]));
  invalid.interrupt();
  expect(() => invalid.content()).toThrow();
  invalid.release();
});

it('快照原子发布不覆盖固定文件，读取校验权限和路径，清理拒绝未知文件与 symlink', async () => {
  const parent = resolve('../../.runtime-test/c04-m1-snapshots');
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, 'files-'));
  const files = new SnapshotFiles(root);
  await files.validate();
  const id = randomUUID(),
    date = new Date('2026-09-21T00:00:00Z');
  const memory = new SnapshotMemory(),
    capture = new Capture(memory);
  capture.append(Buffer.from('你好\n'));
  const result = await files.publish(id, date, 'request', capture);
  expect(result.state).toBe('complete');
  expect((await files.read(id, date, 'request'))?.body.toString()).toBe('你好\n');
  expect((await lstat(join(root, result.path!))).mode & 0o777).toBe(0o600);
  await expect(files.publish(id, date, 'request', capture)).rejects.toThrow();
  expect((await readFile(join(root, result.path!))).toString()).toBe('你好\n');
  await files.clean(id, date, true);
  await symlink(join(root, result.path!), join(root, '2026/09/21', id, 'response.txt'));
  await expect(files.read(id, date, 'response')).rejects.toThrow();
  await expect(files.clean(id, date)).rejects.toThrow();
  capture.release();
});
