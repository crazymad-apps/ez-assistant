import { useEffect, useMemo, useState } from 'react';
import { PagedList } from '../request/paged-list';
import type { PagedSnapshot } from '../request/paged-list';

/** 复用查询取消与代次守卫，公开入口没有后台身份 owner。 */
export function useSiteQuery<T>(read: (signal: AbortSignal) => Promise<T>, identity?: string) {
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{ owner: object; snapshot: PagedSnapshot<T> }>();
  const query = useMemo(
    () =>
      new PagedList(
        async (_: null, signal: AbortSignal) => ({ items: [await read(signal)], total: 1 }),
        () => true,
      ),
    [read, identity],
  );
  useEffect(() => {
    void query.load(null, (patch) =>
      setResult((previous) => ({
        owner: query,
        snapshot: {
          ...(previous?.owner === query ? previous.snapshot : { items: [], total: 0, state: 'loading', error: '' }),
          ...patch,
        },
      })),
    );
    return () => query.cancel();
  }, [query, attempt]);
  const snapshot = result?.owner === query ? result.snapshot : undefined;
  return {
    value: snapshot?.items[0],
    loading: !snapshot || snapshot.state === 'loading',
    error: snapshot?.error ?? '',
    reload: () => setAttempt((n) => n + 1),
  };
}
