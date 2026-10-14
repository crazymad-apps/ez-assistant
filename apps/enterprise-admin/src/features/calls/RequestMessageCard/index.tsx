import { useId, useState } from 'react';
import { Alert, Button, Card, Typography } from 'antd';
import type { Block } from '../BodyReader/projection';
import { RawText } from '../RawText';
import styles from './index.module.scss';

export function RequestMessageCard({ block, text }: Readonly<{ block: Block; text: string }>) {
  const [sourceOpened, setSourceOpened] = useState(false);
  const sourceId = useId();
  return (
    <Card size="small">
      <div className={styles.content}>
        <div className={styles.header}>
          <div className={styles.identity}>
            <Typography.Text strong>{block.title}</Typography.Text>
            {block.detail && <Typography.Text type="secondary">{block.detail}</Typography.Text>}
          </div>
          <Button
            type="text"
            size="small"
            aria-expanded={sourceOpened}
            aria-controls={sourceId}
            aria-label={`${block.title} · ${sourceOpened ? '收起' : '查看'}请求原文`}
            onClick={() => setSourceOpened((opened) => !opened)}
          >
            {sourceOpened ? '收起原文' : '原文'}
          </Button>
        </div>
        {block.status && <Typography.Text type="secondary">{block.status}</Typography.Text>}
        {block.warning && <Alert type="warning" showIcon title={block.warning} />}
        {block.text !== undefined && <RawText text={block.text} compact label={block.title} />}
        <div id={sourceId} hidden={!sourceOpened}>
          {sourceOpened && (
            <div className={styles.content}>
              <Typography.Text type="secondary">来源：完整请求 JSON，保留消息顺序及关联信息。</Typography.Text>
              {/* 请求消息由同一 JSON 投影，原文必须保留完整上下文，不伪造消息字节区间。 */}
              <RawText text={text} label="请求原文" />
            </div>
          )}
        </div>
      </div>
    </Card>
  );
}
