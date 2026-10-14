import { expect, it } from 'vitest';
import { createReadQueue } from '../../src/features/calls/BodyReader/readQueue';

it('正文顺序读取，失败不阻塞后续；已取消的等待项不发请求', async () => {
  const enqueue = createReadQueue(),
    active = new AbortController(),
    cancelled = new AbortController();
  let release!: () => void;
  const pending = new Promise<void>((resolve) => {
    release = resolve;
  });
  const visited: string[] = [];
  const first = enqueue(async () => {
    visited.push('request');
    await pending;
    throw Error('network');
  }, active.signal);
  const skipped = enqueue(async () => {
    visited.push('cancelled');
  }, cancelled.signal);
  const last = enqueue(async () => {
    visited.push('response');
    return 'body';
  }, active.signal);
  const firstResult = expect(first).rejects.toThrow('network'),
    skipResult = expect(skipped).rejects.toThrow();
  cancelled.abort();
  await Promise.resolve();
  await Promise.resolve();
  expect(visited).toEqual(['request']);
  release();
  await firstResult;
  await skipResult;
  expect(await last).toBe('body');
  expect(visited).toEqual(['request', 'response']);
});
