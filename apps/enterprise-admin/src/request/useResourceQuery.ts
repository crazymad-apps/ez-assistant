import { useEffect, useMemo, useState } from 'react';
import adminStore from '../stores/AdminStore';
import { PagedList } from './paged-list';
import type { PagedSnapshot } from './paged-list';

/** 管理页面复用 PagedList 的身份与取消守卫；读取函数由调用方稳定持有。 */
export function useResourceQuery<T>(read: (token: string, signal: AbortSignal) => Promise<T>) {
  const token = adminStore.token;
  const [attempt, setAttempt] = useState(0);
  const empty: PagedSnapshot<T> = { items: [], total: 0, state: 'loading', error: '' };
  const [result, setResult] = useState<{ owner?: object; snapshot: PagedSnapshot<T> }>({ snapshot: empty });
  const query = useMemo(
    () =>
      new PagedList(
        async (_: null, signal: AbortSignal) => ({ items: [await read(token, signal)], total: 1 }),
        () => adminStore.token === token,
      ),
    [token, read],
  );
  useEffect(() => {
    void query.load(null, (patch) =>
      setResult((previous) => ({
        owner: query,
        snapshot: {
          ...(previous.owner === query ? previous.snapshot : { items: [], total: 0, state: 'loading', error: '' }),
          ...patch,
        },
      })),
    );
    return () => query.cancel();
  }, [query, attempt]);
  const snapshot = result.owner === query ? result.snapshot : empty;
  return {
    value: snapshot.items[0],
    loading: snapshot.state === 'loading',
    error: snapshot.error,

    reload: () => setAttempt((n) => n + 1),
  };
}
