import { useEffect, useState } from 'react';
import { Badge, Button, Card, Descriptions, Result, Space, Spin } from 'antd';
import { ArrowLeftOutlined } from '@ant-design/icons';
import { useNavigate, useParams } from 'react-router';
import adminStore from '../../../stores/AdminStore';
import { api, failure } from '../../../request/api';
import { auditView } from '../../../model';
import type { AuditView } from '../../../model';
import styles from './index.module.scss';

/** 详情按 URL 的 ID 独立查询，不把审计正文塞进浏览器 history，也不依赖当前列表页。 */
export function AuditDetailRoute() {
  const { id = '' } = useParams();
  const navigate = useNavigate();
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<{ id: string; record?: AuditView; error?: string }>();
  const validId = /^[1-9][0-9]{0,9}$/.test(id) && Number(id) <= 2147483647;
  useEffect(() => {
    if (!validId) return;
    const request = new AbortController();
    const token = adminStore.token;
    setResult(undefined);
    void api.audit(token, { id: Number(id), limit: 1, offset: 0 }, request.signal).then(
      (page) => {
        if (!request.signal.aborted && adminStore.token === token)
          setResult({ id, record: page.items[0] ? auditView(page.items[0]) : undefined });
      },
      (error: unknown) => {
        if (!request.signal.aborted && adminStore.token === token) setResult({ id, error: failure(error).text });
      },
    );
    return () => request.abort();
  }, [id, validId, attempt]);
  const back = <Button onClick={() => void navigate('/audit')}>返回列表</Button>;
  if (!validId) return <Result status="404" title="审计记录不存在" extra={back} />;
  if (!result || result.id !== id)
    return (
      <Spin tip="正在加载审计详情">
        <div />
      </Spin>
    );
  if (result.error)
    return (
      <Result
        status="warning"
        title="暂时无法加载审计详情"
        subTitle={result.error}
        extra={
          <Space>
            {back}
            <Button onClick={() => setAttempt((value) => value + 1)}>重试</Button>
          </Space>
        }
      />
    );
  const selected = result.record;
  if (!selected) return <Result status="404" title="审计记录不存在" extra={back} />;
  return (
    <section className={styles.page}>
      <Card
        variant="borderless"
        className={styles.detail}
        title={
          <Space size={8}>
            <Button
              type="text"
              size="small"
              icon={<ArrowLeftOutlined />}
              aria-label="返回列表"
              onClick={() => void navigate('/audit')}
            />
            审计详情
          </Space>
        }
        extra={
          <Badge status={selected.success ? 'success' : 'error'} text={selected.success ? '操作成功' : '操作失败'} />
        }
      >
        <Descriptions
          column={1}
          items={[
            { key: 'action', label: '操作', children: selected.action },
            {
              key: 'time',
              label: '发生时间',
              children: `${selected.time}（${Intl.DateTimeFormat().resolvedOptions().timeZone}）`,
            },
            { key: 'actor', label: '操作者用户名', children: selected.actor },
            { key: 'target', label: '目标用户名', children: selected.target },
            { key: 'result', label: '结果', children: selected.success ? '成功' : '失败' },
            { key: 'id', label: '请求关联 ID', children: selected.requestId },
            { key: 'detail', label: '操作说明', children: selected.detail },
          ]}
        />
      </Card>
    </section>
  );
}
