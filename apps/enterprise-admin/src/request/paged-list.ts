import { failure } from './api';
import type { LoadState } from '../model';

export type PagedPage<TItem> = Readonly<{ items: readonly TItem[]; total: number }>;
export type PagedSnapshot<TItem> = Readonly<{
  items: readonly TItem[];
  total: number;
  state: LoadState;
  error: string;
}>;

/**
 * 页面级分页列表读取器：路由页面本地使用，不进入全局 Store。
 * 新查询中止旧请求并成为唯一可提交代次；响应提交前仍校验调用方身份快照，
 * 卸载或身份切换后的迟到响应不能写入页面。取消请求不代表服务端回滚。
 */
export class PagedList<TQuery, TItem> {
  private request?: AbortController;
  constructor(
    private readonly read: (query: TQuery, signal: AbortSignal) => Promise<PagedPage<TItem>>,
    private readonly isCurrent: () => boolean,
  ) {}
  cancel() {
    this.request?.abort();
  }
  async load(query: TQuery, apply: (patch: Partial<PagedSnapshot<TItem>>) => void) {
    if (!this.isCurrent()) return;
    this.request?.abort();
    const request = new AbortController();
    this.request = request;
    apply({ state: 'loading', error: '' });
    const commit = (patch: Partial<PagedSnapshot<TItem>>) => {
      // 只有未被取代且身份未变的本次响应可以提交；读取进行中保留旧数据，不清空列表。
      if (this.isCurrent() && this.request === request && !request.signal.aborted) apply(patch);
    };
    try {
      const page = await this.read(query, request.signal);
      commit({ items: page.items, total: page.total, state: 'normal' });
    } catch (error) {
      commit({ state: 'failure', error: failure(error).text });
    }
  }
}
