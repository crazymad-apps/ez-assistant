import { useEffect, useMemo, useState } from 'react';
import adminStore from '../../../stores/AdminStore';
import { api } from '../../../request/api';
import type { UserQuery } from '../../../request/api';
import { PagedList } from '../../../request/paged-list';
import type { PagedSnapshot } from '../../../request/paged-list';
import { userView } from '../../../model';
import type { UserView } from '../../../model';

/**
 * 用户列表页本地数据：查询条件、分页与加载态留在路由页面，不进入全局 Store。
 * Token 在组件挂载时快照；身份切换由路由守卫整树卸载，迟到响应由 PagedList 代次守卫丢弃。
 * 写入成功后由页面直接调用 onRecover 回调重载列表。
 */
export function useUsersData() {
  const token = adminStore.token;
  const [query, setQuery] = useState<UserQuery>({ limit: 20, offset: 0 });
  const [attempt, setAttempt] = useState(0);
  const [snapshot, setSnapshot] = useState<PagedSnapshot<UserView>>({
    items: [],
    total: 0,
    state: 'loading',
    error: '',
  });
  const list = useMemo(
    () =>
      new PagedList(
        async (current: UserQuery, signal: AbortSignal) => {
          const page = await api.users(token, current, signal);
          return { items: page.items.map(userView), total: page.total };
        },
        () => adminStore.token === token,
      ),
    [token],
  );
  // 查询变化与手动重试/写入后刷新共用同一读取入口。
  useEffect(() => {
    void list.load(query, (patch) => setSnapshot((previous) => ({ ...previous, ...patch })));
    return () => list.cancel();
  }, [list, query, attempt]);
  return {
    users: snapshot.items,
    preview: snapshot.state,
    total: snapshot.total,
    error: snapshot.error,
    query,
    onQuery: setQuery,
    onRecover: () => setAttempt((value) => value + 1),
  };
}
