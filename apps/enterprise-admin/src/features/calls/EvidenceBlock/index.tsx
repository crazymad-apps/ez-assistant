import { useState } from 'react';
import { Alert, Button, Collapse, Space, Tag, Typography } from 'antd';
import type { Block } from '../BodyReader/projection';
import { RawText } from '../RawText';
import styles from './index.module.scss';

export function EvidenceBlock({ block, text }: Readonly<{ block: Block; text: string }>) {
  const [sourcesVisible, setSourcesVisible] = useState(50);
  return (
    <div className={styles.block}>
      <Space wrap>
        <Typography.Text strong>
          {block.title}
          {block.continuation ? ' · 续段' : ''}
        </Typography.Text>
        {block.status && <Tag>{block.status}</Tag>}
        {!!block.deltaCount && <Typography.Text type="secondary">聚合 {block.deltaCount} 条增量事件</Typography.Text>}
      </Space>
      <Typography.Text type="secondary">{block.sources[0]?.type}</Typography.Text>
      {block.detail && (
        <Typography.Text type="secondary" className={styles.detail}>
          {block.detail}
        </Typography.Text>
      )}
      {block.warning && <Alert type="warning" showIcon title={block.warning} />}
      {block.conflict !== undefined && <Typography.Text strong>增量内容</Typography.Text>}
      {block.text !== undefined && <RawText text={block.text} compact label={block.title} />}
      {block.conflict !== undefined && (
        <>
          <Typography.Text strong>结束事件内容</Typography.Text>
          <RawText text={block.conflict} compact label="结束事件内容" />
        </>
      )}
      <Collapse
        size="small"
        ghost
        destroyOnHidden
        items={[
          {
            key: 'sources',
            label: `查看来源（${block.sources.length} 条）`,
            children: (
              <div className={styles.sources}>
                {block.sources.slice(0, sourcesVisible).map((source, i) => (
                  <Collapse
                    key={`${source.number}:${i}`}
                    size="small"
                    destroyOnHidden
                    items={[
                      {
                        key: 'raw',
                        label: `#${source.number} ${source.type} · 字符 ${source.start}–${source.end}`,
                        children: (
                          <RawText text={text.slice(source.start, source.end)} label={`事件 ${source.number} 原文`} />
                        ),
                      },
                    ]}
                  />
                ))}
                {block.sources.length > sourcesVisible && (
                  <Button size="small" onClick={() => setSourcesVisible((n) => n + 50)}>
                    继续显示来源
                  </Button>
                )}
                <Typography.Text type="secondary">
                  字符区间从 0 开始，按 UTF-16 计数；末尾位置不包含在内。
                </Typography.Text>
              </div>
            ),
          },
        ]}
      />
    </div>
  );
}
