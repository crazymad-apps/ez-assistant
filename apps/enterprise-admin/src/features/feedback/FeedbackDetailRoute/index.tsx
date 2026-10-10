import { useCallback, useState } from 'react';
import { useNavigate, useParams } from 'react-router';
import { Alert, Button, Card, Descriptions, Space, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { feedbackApi } from '../../../request/feedback';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { HandlingDialog } from '../HandlingDialog';
import { kindLabels, sourceLabels, statusLabels } from '../model';
import styles from './index.module.scss';

export const FeedbackDetailRoute = observer(function FeedbackDetailRoute() {
  const { id = '' } = useParams();
  const [handlingOpen, setHandlingOpen] = useState(false);
  const navigate = useNavigate();
  const read = useCallback((token: string, signal: AbortSignal) => feedbackApi.get(token, id, signal), [id]);
  const result = useResourceQuery(read);
  const row = result.value;
  return (
    <div className={styles.page}>
      <Card>
        <Space>
          <Button disabled={adminStore.busy} onClick={() => void navigate('/feedback')}>
            返回反馈列表
          </Button>
          <Button disabled={adminStore.busy || result.loading} onClick={result.reload}>
            刷新详情
          </Button>
          <Button type="primary" disabled={adminStore.busy || !row} onClick={() => setHandlingOpen(true)}>
            处理反馈
          </Button>
        </Space>
      </Card>
      {result.error && <Alert type="error" showIcon title={result.error} />}
      {result.loading && !row && <Spin />}
      {row && (
        <>
          <Card title={row.title} classNames={{ body: styles.detail_content }}>
            <Descriptions
              column={{ xs: 1, sm: 1, lg: 2 }}
              items={[
                { key: 'id', label: '反馈编号', children: row.id },
                { key: 'status', label: '状态', children: statusLabels[row.status] },
                {
                  key: 'owner',
                  label: '提交者',
                  children: `${row.owner_display_name} / ${row.owner_username} (${row.owner_user_id})`,
                },
                { key: 'source', label: '来源', children: sourceLabels[row.source] },
                { key: 'created', label: '提交时间', children: new Date(row.created_at).toLocaleString() },
                { key: 'updated', label: '更新时间', children: new Date(row.updated_at).toLocaleString() },
                { key: 'target', label: '反馈目标', children: `${kindLabels[row.target.kind]} / ${row.target.name}` },
                { key: 'reference', label: '目标引用', children: row.target.reference ?? '手动描述的对象' },
                { key: 'version', label: '插件版本', children: row.target.version ?? '—' },
                { key: 'content', label: '具体内容标识', children: row.target.content_ref ?? '—' },
                { key: 'location', label: '功能 / 位置', children: row.target.location ?? '—' },
              ]}
            />
            {[
              ['问题描述', row.description],
              ['复现步骤', row.reproduction],
              ['期望结果', row.expected_result],
            ].map(([label, value]) => (
              <section key={label} className={styles.text_section}>
                <Typography.Title level={5} className={styles.section_title}>
                  {label}
                </Typography.Title>
                <Typography.Paragraph className={styles.text}>{value ?? '未填写'}</Typography.Paragraph>
              </section>
            ))}
          </Card>
          <Card title="运行环境">
            <Descriptions
              column={{ xs: 1, sm: 1, lg: 2 }}
              items={[
                {
                  key: 'host',
                  label: 'Host',
                  children: [row.environment.host_version, row.environment.host_os, row.environment.host_arch]
                    .map((v) => v ?? '未知')
                    .join(' / '),
                },
                {
                  key: 'client',
                  label: '客户端',
                  children: [
                    row.environment.client_kind,
                    row.environment.client_version,
                    row.environment.client_os,
                    row.environment.client_arch,
                  ]
                    .map((v) => v ?? '未知')
                    .join(' / '),
                },
              ]}
            />
          </Card>
          <Card title="处理结果">
            <Typography.Paragraph>
              最后处理者：
              {row.handler_display_name
                ? `${row.handler_display_name} / ${row.handler_username} (${row.handler_user_id})`
                : '尚未处理'}
            </Typography.Paragraph>
            <Typography.Paragraph className={styles.text}>{row.handling_note ?? '暂无处理说明'}</Typography.Paragraph>
          </Card>
        </>
      )}
      {handlingOpen && (
        <HandlingDialog key={id} id={id} onClose={() => setHandlingOpen(false)} onSaved={result.reload} />
      )}
    </div>
  );
});
