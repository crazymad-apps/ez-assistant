import { useState } from 'react';
import { Button, Space, Typography } from 'antd';
import { CodePreview } from '../CodePreview';
import styles from './index.module.scss';

/** 只渲染有界文本节点；展开不执行 HTML、不加载正文中的资源。 */
export function RawText({
  text,
  label = '原文',
  compact = false,
}: Readonly<{ text: string; label?: string; compact?: boolean }>) {
  const step = 2000;
  const [visible, setVisible] = useState(step);
  if (!compact || /^\s*[{[]/.test(text)) return <CodePreview text={text} label={label} />;
  return (
    <div className={styles.container}>
      <pre className={styles.preview} tabIndex={0} aria-label={label} role="region">
        {text.slice(0, visible)}
      </pre>
      {text.length > visible && (
        <Space wrap>
          <Typography.Text type="secondary">内容预览，尚未展示全文</Typography.Text>
          <Button size="small" onClick={() => setVisible((previous) => previous + step)}>
            继续显示内容
          </Button>
        </Space>
      )}
    </div>
  );
}
