import { useEffect, useMemo, useState } from 'react';
import adminStore from '../../../stores/AdminStore';
import { api } from '../../../request/api';
import type { AuditQuery } from '../../../request/api';
import { PagedList } from '../../../request/paged-list';
import type { PagedSnapshot } from '../../../request/paged-list';
import { auditView } from '../../../model';
import type { AuditView } from '../../../model';

/**
 * 审计列表页本地数据：查询条件、分页与加载态留在路由页面，不进入全局 Store。
 * Token 在组件挂载时快照；操作者检索直接使用生成 SDK 的分页接口，不维护平行缓存。
 */
export function useAuditData() {
  const token = adminStore.token;
  const [query, setQuery] = useState<AuditQuery>({ limit: 20, offset: 0 });
  const [attempt, setAttempt] = useState(0);
  const [snapshot, setSnapshot] = useState<PagedSnapshot<AuditView>>({
    items: [],
    total: 0,
    state: 'loading',
    error: '',
  });
  const list = useMemo(
    () =>
      new PagedList(
        async (current: AuditQuery, signal: AbortSignal) => {
          const page = await api.audit(token, current, signal);
          return { items: page.items.map(auditView), total: page.total };
        },
        () => adminStore.token === token,
      ),
    [token],
  );
  useEffect(() => {
    void list.load(query, (patch) => setSnapshot((previous) => ({ ...previous, ...patch })));
    return () => list.cancel();
  }, [list, query, attempt]);
  const findActors = useMemo(
    () => async (search: string, signal: AbortSignal) => {
      const page = await api.users(token, { search: search || undefined, limit: 20, offset: 0 }, signal);
      if (adminStore.token !== token || signal.aborted) return [];
      return page.items.map((user) => ({ value: user.id, label: `${user.display_name} · ${user.username}` }));
    },
    [token],
  );
  return {
    records: snapshot.items,
    preview: snapshot.state,
    total: snapshot.total,
    error: snapshot.error,
    query,
    onQuery: setQuery,
    onRecover: () => setAttempt((value) => value + 1),
    findActors,
  };
}
