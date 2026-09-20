import { describe, expect, it, vi } from 'vitest';
import { AxiosError, AxiosHeaders } from 'axios';
import { PagedList } from '../../src/request/paged-list';
import type { PagedSnapshot } from '../../src/request/paged-list';

type Item = { id: number };
type Query = { offset: number };
const page = (items: Item[], total = items.length) => ({ items, total });
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function collector<TItem>() {
  const patches: Partial<PagedSnapshot<TItem>>[] = [];
  return { patches, apply: (patch: Partial<PagedSnapshot<TItem>>) => patches.push(patch) };
}

describe('页面级分页读取', () => {
  it('新查询中止旧请求并胜出，即使旧请求忽略 abort 也不能覆盖', async () => {
    const old = deferred<ReturnType<typeof page>>();
    const read = vi
      .fn()
      .mockReturnValueOnce(old.promise)
      .mockResolvedValueOnce(page([{ id: 1 }]));
    const list = new PagedList<Query, Item>(read, () => true);
    const { patches, apply } = collector<Item>();
    const pending = list.load({ offset: 0 }, apply);
    const oldSignal = read.mock.calls[0]![1] as AbortSignal;
    await list.load({ offset: 20 }, apply);
    old.resolve(page([], 99));
    await pending;
    expect(oldSignal.aborted).toBe(true);
    expect(patches.at(-1)).toEqual({ items: [{ id: 1 }], total: 1, state: 'normal' });
  });
  it('身份失效后的迟到响应不能提交页面', async () => {
    const request = deferred<ReturnType<typeof page>>();
    let current = true;
    const list = new PagedList<Query, Item>(
      () => request.promise,
      () => current,
    );
    const { patches, apply } = collector<Item>();
    const pending = list.load({ offset: 0 }, apply);
    current = false;
    request.resolve(page([{ id: 1 }]));
    await pending;
    // 只有进入加载态的一次提交，迟到数据被丢弃。
    expect(patches).toEqual([{ state: 'loading', error: '' }]);
  });
  it('读取失败进入 failure 态并给出文案，不残留加载态', async () => {
    const list = new PagedList<Query, Item>(
      async () => {
        throw new AxiosError('unsafe raw error', undefined, undefined, undefined, {
          data: { error: { code: 'TOKEN_INVALID', message: 'secret' } },
          status: 401,
          statusText: 'Unauthorized',
          headers: {},
          config: { headers: new AxiosHeaders() },
        });
      },
      () => true,
    );
    const { patches, apply } = collector<Item>();
    await list.load({ offset: 0 }, apply);
    expect(patches.at(-1)?.state).toBe('failure');
    expect(patches.at(-1)?.error).toContain('登录已失效');
    expect(patches.at(-1)?.error).not.toContain('secret');
  });
  it('卸载取消在途请求，其响应不再提交', async () => {
    const request = deferred<ReturnType<typeof page>>();
    const list = new PagedList<Query, Item>(
      () => request.promise,
      () => true,
    );
    const { patches, apply } = collector<Item>();
    const pending = list.load({ offset: 0 }, apply);
    list.cancel();
    request.resolve(page([{ id: 1 }]));
    await pending;
    expect(patches).toEqual([{ state: 'loading', error: '' }]);
  });
});
