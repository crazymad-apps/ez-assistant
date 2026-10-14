import { useEffect, useMemo, useState, type CSSProperties } from 'react';
import { Button, Checkbox, Segmented, Space, Typography, theme } from 'antd';
import { codeTokens, prepareCodePreview, type CodePreviewValue } from './preview';
import styles from './index.module.scss';

export function CodePreview({ text, label }: Readonly<{ text: string; label: string }>) {
  const { token } = theme.useToken();
  const [preview, setPreview] = useState<{ source: string; value: CodePreviewValue }>();
  const [mode, setMode] = useState('formatted');
  const [wrap, setWrap] = useState(true);
  const [visible, setVisible] = useState(32768);
  useEffect(() => {
    const controller = new AbortController();
    setMode('formatted');
    setVisible(32768);
    void prepareCodePreview(text, controller.signal).then(
      (value) => {
        if (!controller.signal.aborted) setPreview({ source: text, value });
      },
      () => {
        if (!controller.signal.aborted) setPreview({ source: text, value: { notice: '无法格式化，保留原文。' } });
      },
    );
    return () => controller.abort();
  }, [text]);
  const value = preview?.source === text ? preview.value : undefined;
  const formatted = mode === 'formatted' && value?.formatted !== undefined;
  const content = formatted ? value.formatted! : text;
  const shown = content.slice(0, visible);
  const tokens = useMemo(() => codeTokens(shown), [shown]);
  const palette = {
    '--code-bg': token.colorFillQuaternary,
    '--code-border': token.colorBorderSecondary,
    '--code-radius': `${token.borderRadius}px`,
    '--code-font': token.fontFamilyCode,
    '--code-size': `${token.fontSizeSM}px`,
    '--code-key': token.colorPrimaryText,
    '--code-string': token.colorSuccessText,
    '--code-number': token.colorWarningText,
    '--code-literal': token.colorErrorText,
  } as CSSProperties;
  return (
    <div className={styles.container} style={palette}>
      <div className={styles.toolbar}>
        <Space wrap size="small">
          <Typography.Text type="secondary">{formatted ? 'JSON' : '原文'}</Typography.Text>
          {value?.formatted !== undefined && (
            <Segmented
              size="small"
              aria-label="代码显示方式"
              value={mode}
              options={[
                { label: '格式化', value: 'formatted' },
                { label: '原文', value: 'raw' },
              ]}
              onChange={(next) => {
                setMode(next);
                setVisible(32768);
              }}
            />
          )}
        </Space>
        <Checkbox checked={wrap} onChange={(event) => setWrap(event.target.checked)}>
          自动换行
        </Checkbox>
      </div>
      {formatted && value.event && <div className={styles.event}>event: {value.event} · data</div>}
      {value?.notice && (
        <Typography.Text type="secondary" className={styles.notice}>
          {value.notice}
        </Typography.Text>
      )}
      <pre className={styles.code} data-wrap={wrap} tabIndex={0} aria-label={label} role="region">
        <code>
          {tokens.map((part) => (
            <span key={part.start} data-tone={part.tone}>
              {shown.slice(part.start, part.end)}
            </span>
          ))}
        </code>
      </pre>
      {content.length > visible && (
        <Space wrap className={styles.footer}>
          <Typography.Text type="secondary">内容预览，尚未展示全文</Typography.Text>
          <Button size="small" onClick={() => setVisible((previous) => previous + 32768)}>
            继续显示内容
          </Button>
        </Space>
      )}
    </div>
  );
}
