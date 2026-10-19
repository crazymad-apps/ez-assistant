import { useCallback, useEffect, useRef, useState } from 'react';
import { Alert, Button, Collapse, Empty, Pagination, Spin } from 'antd';
import { observer } from 'mobx-react-lite';
import type { ListHostDevicesData } from '../../../request/openapi';
import { devicesApi } from '../../../request/devices';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { DeviceCard } from '../DeviceCard';
import styles from './index.module.scss';

export const DeviceSection = observer(function DeviceSection(props: {
  status: ListHostDevicesData['query']['status'];
  title: string;
  search: string;
  refresh: number;
  onEdit: (id: string, reject?: boolean) => void;
}) {
  const [page, setPage] = useState(1);
  const [expanded, setExpanded] = useState(true);
  // 搜索改变时由父级 key 重建分页；折叠状态仍独立于刷新结果。
  const read = useCallback(
    (token: string, signal: AbortSignal) =>
      devicesApi.list(
        token,
        { status: props.status, search: props.search, offset: (page - 1) * 20, limit: 20 },
        signal,
      ),
    [props.status, props.search, page],
  );
  const result = useResourceQuery(read);
  useEffect(() => {
    if (!result.value || result.loading || result.error) return;
    // 条目减少后回到有效页，避免隐藏分页后停留在空的越界页。
    const lastPage = Math.max(1, Math.ceil(result.value.total / 20));
    if (page > lastPage) setPage(lastPage);
  }, [result.value, result.loading, result.error, page]);
  const previousRefresh = useRef(props.refresh);
  useEffect(() => {
    if (previousRefresh.current !== props.refresh && !result.loading) {
      previousRefresh.current = props.refresh;
      result.reload();
    }
  }, [props.refresh, result.loading, result.reload]);
  const emptyText = props.status === 'unregistered' ? '暂无在线的未登记设备' : '暂无已登记设备';
  return (
    <Collapse
      activeKey={expanded ? ['devices'] : []}
      onChange={(keys) => setExpanded(keys.length > 0)}
      items={[
        {
          key: 'devices',
          label: `${props.title}（${result.value?.total ?? '…'}）`,
          children: (
            <>
              {result.error && (
                <Alert
                  type="error"
                  title={`状态未更新：${result.error}`}
                  action={
                    <Button onClick={result.reload} disabled={result.loading}>
                      重试
                    </Button>
                  }
                />
              )}
              {result.loading && !result.value && <Spin />}
              {result.value?.items.length === 0 && <Empty description={props.search ? '没有匹配的设备' : emptyText} />}
              <div className={styles.grid}>
                {result.value?.items.map((device) => (
                  <DeviceCard key={device.id} device={device} disabled={adminStore.busy} onEdit={props.onEdit} />
                ))}
              </div>
              {!!result.value?.total && (
                <Pagination
                  current={page}
                  total={result.value.total}
                  pageSize={20}
                  showSizeChanger={false}
                  hideOnSinglePage
                  onChange={setPage}
                  disabled={result.loading || adminStore.busy}
                />
              )}
            </>
          ),
        },
      ]}
    />
  );
});
