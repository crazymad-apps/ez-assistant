import { useId, useState } from 'react';
import { Alert, Button, Space, Timeline, Typography } from 'antd';
import type { Projection } from '../BodyReader/projection';
import { EvidenceBlock } from '../EvidenceBlock';
import { RequestMessageCard } from '../RequestMessageCard';
import styles from './index.module.scss';

export function EvidenceBlocks({
  projection,
  text,
  kind,
}: Readonly<{ projection: Projection; text: string; kind: 'request' | 'response' }>) {
  const [visible, setVisible] = useState(100);
  const firstWarning = projection.blocks.findIndex((block) => !!block.warning);
  const prefix = useId();
  const [target, setTarget] = useState<number | null>(null);
  // ref 在追加渲染后定位真实节点；仅显式点击触发，不因解析或刷新抢走焦点。
  const content = (i: number) => (
    <div
      id={`${prefix}-${i}`}
      tabIndex={-1}
      ref={(element) => {
        if (element && target === i) {
          element.scrollIntoView({ block: 'center' });
          element.focus({ preventScroll: true });
          setTarget(null);
        }
      }}
    >
      {kind === 'request' ? (
        <RequestMessageCard block={projection.blocks[i]!} text={text} />
      ) : (
        <EvidenceBlock block={projection.blocks[i]!} text={text} />
      )}
    </div>
  );
  return (
    <div className={kind === 'request' ? `${styles.blocks} ${styles.requests}` : styles.blocks}>
      {projection.notices.map((notice, i) => (
        <Alert key={i} type="info" showIcon title={notice} />
      ))}
      {firstWarning >= 0 && (
        <Button
          size="small"
          onClick={() => {
            setVisible((n) => Math.max(n, firstWarning + 1));
            setTarget(firstWarning);
          }}
        >
          定位首个异常
        </Button>
      )}
      {projection.stream ? (
        <Timeline
          items={projection.blocks
            .slice(0, visible)
            .map((block, i) => ({ key: i, color: block.warning ? 'red' : 'gray', content: content(i) }))}
        />
      ) : (
        projection.blocks.slice(0, visible).map((_, i) => (
          <div className={kind === 'request' ? undefined : styles.message} key={i}>
            {content(i)}
          </div>
        ))
      )}
      {projection.blocks.length > visible && (
        <Space wrap>
          <Typography.Text type="secondary">
            已展示 {visible} / {projection.blocks.length}
          </Typography.Text>
          <Button onClick={() => setVisible((n) => n + 100)}>继续显示后续{projection.stream ? '事件' : '内容'}</Button>
        </Space>
      )}
    </div>
  );
}
