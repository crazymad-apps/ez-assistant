import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readdir, rm } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { PassThrough, Readable } from 'node:stream';
import { afterEach, expect, it } from 'vitest';
import { DesktopReleaseFiles } from '../../dist/desktop-releases/files.js';

const roots: string[] = [];
afterEach(async () => { for (const path of roots.splice(0)) await rm(path, { recursive: true }); });
async function setup() {
  const parent = resolve('../../.runtime-test'); await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, 'f01-files-')); roots.push(root);
  const files = new DesktopReleaseFiles(root); await files.initialize();
  return { root, files };
}
const expected = { name: 'test.msi', purposes: ['installer', 'updater'] as ('installer' | 'updater')[], size: 3,
  sha256: createHash('sha256').update('abc').digest('hex'), signature: 'test' };

it('超出清单长度、截断、摘要不符都清除临时文件；正确重传可接纳', async () => {
  const { root, files } = await setup();
  for (const bytes of ['abcd', 'ab', 'xyz']) {
    await expect(files.run(signal => files.receive(Readable.from([Buffer.from(bytes)]), expected, signal), new AbortController().signal)).rejects.toThrow();
    expect(await readdir(root)).toEqual([]);
  }
  await files.run(signal => files.receive(Readable.from([Buffer.from('abc')]), expected, signal), new AbortController().signal);
  expect(await files.available([expected])).toBe(true);
  expect(await readdir(root)).toEqual([expected.sha256]);
  await files.close();
});

it('客户端中断未完成的上传后，关闭 owner 会等待清理完成', async () => {
  const { root, files } = await setup();
  const client = new AbortController(), stream = new PassThrough();
  const work = files.run(signal => files.receive(stream, expected, signal), client.signal);
  const rejected = expect(work).rejects.toThrow();
  stream.write('a');
  // 等待文件已创建，覆盖真正处于流式写入中的取消。
  while (!(await readdir(root)).length) await new Promise(resolve => setImmediate(resolve));
  client.abort();
  await rejected; await files.close();
  expect(await readdir(root)).toEqual([]);
});

it('两路上传占满后拒绝新请求，关闭时取消并等待现有任务', async () => {
  const { files } = await setup();
  const wait = (signal: AbortSignal) => new Promise<void>((_, reject) => {
    signal.throwIfAborted(); signal.addEventListener('abort', () => reject(signal.reason), { once: true });
  });
  const one = files.run(wait, new AbortController().signal), two = files.run(wait, new AbortController().signal);
  const settled = Promise.allSettled([one, two]);
  await expect(files.run(async () => {}, new AbortController().signal)).rejects.toMatchObject({ status: 429 });
  await files.close();
  expect((await settled).map(result => result.status)).toEqual(['rejected', 'rejected']);
});
