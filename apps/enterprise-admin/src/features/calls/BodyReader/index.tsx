import { useMemo, useRef, useState } from 'react';
import { Button, Card, Space, Typography } from 'antd';
import type { CallDetails, CallSnapshot } from '../../../request/openapi';
import { SnapshotPanel } from '../SnapshotPanel';
import { snapshotLabel } from '../model';
import styles from './index.module.scss';
import { createReadQueue } from './readQueue';

export const readable = (snapshot: CallSnapshot) => ['complete', 'partial'].includes(snapshot.state);
export const snapshotKey = (snapshot: CallSnapshot) =>
  `${snapshot.state}:${snapshot.sha256}:${snapshot.bytes}:${snapshot.reason}`;

export function BodyReader({ row }: Readonly<{ row: CallDetails }>) {
  const [opened, setOpened] = useState(false);
  const button = useRef<HTMLButtonElement>(null);
  const enqueue = useMemo(createReadQueue, []);
  const any = readable(row.request) || readable(row.response);
  return (
    <Card
      title={
        <div className={styles.header}>
          <span>正文与事件</span>
          <Button
            ref={button}
            disabled={!any && !opened}
            onClick={() => {
              setOpened((value) => !value);
              button.current?.focus();
            }}
          >
            {opened ? '收起正文' : '查看正文'}
          </Button>
        </div>
      }
    >
      <div className={styles.container}>
        <div className={styles.columns}>
          {(['request', 'response'] as const).map((side) => (
            <section className={styles.side} key={side} aria-label={side === 'request' ? '请求消息' : '响应过程'}>
              <Space direction="vertical" size={4}>
                <Typography.Title level={5}>{side === 'request' ? '请求消息' : '响应过程'}</Typography.Title>
                <Typography.Text type="secondary">{snapshotLabel(row[side])}</Typography.Text>
              </Space>
              <SnapshotPanel
                key={`${side}:${snapshotKey(row[side])}:${row.response_content_type}:${row.response_content_encoding}:${opened}`}
                id={row.id}
                side={side}
                snapshot={row[side]}
                row={row}
                opened={opened}
                enqueue={enqueue}
              />
            </section>
          ))}
        </div>
        {row.upstream_failure && (
          <section className={styles.failure} aria-label="独立上游失败帧">
            <SnapshotPanel
              key={`failure:${snapshotKey(row.upstream_failure)}:${opened}`}
              id={row.id}
              side="upstream_failure"
              snapshot={row.upstream_failure}
              row={row}
              enqueue={enqueue}
            />
          </section>
        )}
      </div>
    </Card>
  );
}
