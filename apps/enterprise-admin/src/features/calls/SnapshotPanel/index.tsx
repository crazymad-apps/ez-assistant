import { useCallback, useState } from 'react';
import { Alert, Button, Checkbox, Space, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import type { CallSnapshot } from '../../../request/openapi';
import { snapshotLabels } from '../model';
import styles from './index.module.scss';

/** Tabs 切换时销毁面板；正文只在显式查看后读取，React 文本节点不解释 HTML。 */
export const SnapshotPanel = observer(function SnapshotPanel(
  props: Readonly<{ id: string; side: 'request' | 'response'; snapshot: CallSnapshot }>,
) {
  const [opened, setOpened] = useState(false);
  const [wrap, setWrap] = useState(true);
  const read = useCallback(
    async (token: string, signal: AbortSignal) =>
      opened ? callApi.snapshot(token, props.id, props.side, signal) : null,
    [opened, props.id, props.side],
  );
  const query = useResourceQuery(read);
  const snapshot = query.value ?? props.snapshot;

  return (
    <div className={styles.panel}>
      <Space wrap>
        <Typography.Text>
          {snapshotLabels[snapshot.state]} · {snapshot.bytes ?? 0} 字节{snapshot.reason ? ` · ${snapshot.reason}` : ''}
        </Typography.Text>
        <Button
          disabled={!['complete', 'partial'].includes(props.snapshot.state)}
          loading={query.loading && opened}
          onClick={() => {
            setOpened(true);
            query.reload();
          }}
        >
          查看内容
        </Button>
        <Checkbox checked={wrap} onChange={(event) => setWrap(event.target.checked)}>
          自动换行
        </Checkbox>
      </Space>
      {snapshot.sha256 && <Typography.Text type="secondary">SHA-256：{snapshot.sha256}</Typography.Text>}
      {query.error && <Alert type="error" title={query.error} />}
      {query.value?.text !== null && query.value?.text !== undefined && (
        <pre className={styles.content} data-wrap={wrap}>
          {query.value.text}
        </pre>
      )}
    </div>
  );
});
