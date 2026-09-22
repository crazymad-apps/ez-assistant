import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, Button, Card, DatePicker, Form, Input, InputNumber, Pagination, Select, Space, Table } from 'antd';
import type { TableProps } from 'antd';
import { observer } from 'mobx-react-lite';
import type { ListLlmCallsData, CallDetails } from '../../../request/openapi';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { useElementHeight } from '../../../components/useElementHeight';
import { RecordingDialog } from '../RecordingDialog';
import { outcomeLabels, snapshotLabels } from '../model';
import styles from './index.module.scss';

type Query = NonNullable<ListLlmCallsData['query']>;

export const CallsRoute = observer(function CallsRoute() {
  const [draft, setDraft] = useState<Query>({ limit: 50, offset: 0 });
  const [query, setQuery] = useState<Query>(draft);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const navigate = useNavigate();
  const read = useCallback((token: string, signal: AbortSignal) => callApi.list(token, query, signal), [query]);
  const records = useResourceQuery(read);
  const settings = useResourceQuery(callApi.settings);
  const { ref, height } = useElementHeight<HTMLDivElement>();
  const columns: TableProps<CallDetails>['columns'] = [
    {
      title: '开始时间',
      key: 'time',
      width: 190,

      render: (_: unknown, row: CallDetails) => new Date(row.started_at).toLocaleString(),
    },
    { title: '用户', dataIndex: 'username', width: 120 },
    {
      title: '服务商 / 模型',
      key: 'model',
      width: 220,

      render: (_: unknown, row: CallDetails) => `${row.provider_name ?? '—'} / ${row.model_id ?? '—'}`,
    },
    {
      title: '类型',
      key: 'kind',
      width: 100,

      render: (_: unknown, row: CallDetails) => (row.kind === 'admin_test' ? '配置测试' : '模型调用'),
    },
    {
      title: '转发结果',
      key: 'result',
      width: 120,

      render: (_: unknown, row: CallDetails) => outcomeLabels[row.outcome],
    },
    {
      title: '耗时',
      key: 'duration',
      width: 100,

      render: (_: unknown, row: CallDetails) =>
        row.ended_at ? `${((Date.parse(row.ended_at) - Date.parse(row.started_at)) / 1000).toFixed(1)} 秒` : '—',
    },
    { title: 'HTTP', dataIndex: 'http_status', width: 80 },
    {
      title: '请求 / 响应快照',
      key: 'snapshot',
      width: 180,

      render: (_: unknown, row: CallDetails) =>
        `${snapshotLabels[row.request.state]} / ${snapshotLabels[row.response.state]}`,
    },
    {
      title: '操作',
      key: 'detail',
      width: 80,
      fixed: 'right',
      align: 'center',

      render: (_: unknown, row: CallDetails) => (
        <Button type="link" size="small" onClick={() => navigate(`/calls/${row.id}`)}>
          详情
        </Button>
      ),
    },
  ];

  return (
    <section className={styles.page}>
      <Card
        title="模型调用记录"
        className={styles.card}
        extra={
          <Button disabled={!settings.value} onClick={() => setSettingsOpen(true)}>
            记录策略
          </Button>
        }
      >
        <Form
          layout="inline"
          className={styles.filters}
          onFinish={() => {
            setQuery({ ...draft, offset: 0 });
            records.reload();
          }}
        >
          <Form.Item label="调用 ID" htmlFor="call-filter-0">
            <Input
              id="call-filter-0"
              className={styles.input}
              placeholder="调用 ID"
              value={draft.id ?? ''}
              onChange={(event) => setDraft({ ...draft, id: event.target.value || undefined })}
            />
          </Form.Item>
          <Form.Item label="用户 ID" htmlFor="call-filter-1">
            <InputNumber
              id="call-filter-1"
              placeholder="用户 ID"
              min={1}
              precision={0}
              value={draft.user_id}
              onChange={(value) => setDraft({ ...draft, user_id: value ?? undefined })}
            />
          </Form.Item>
          <Form.Item label="服务商 ID" htmlFor="call-filter-2">
            <Input
              id="call-filter-2"
              className={styles.input}
              placeholder="服务商 ID"
              value={draft.provider_instance_id ?? ''}
              onChange={(event) => setDraft({ ...draft, provider_instance_id: event.target.value || undefined })}
            />
          </Form.Item>
          <Form.Item label="模型 ID" htmlFor="call-filter-3">
            <Input
              id="call-filter-3"
              className={styles.input}
              placeholder="模型 ID"
              value={draft.model_id ?? ''}
              onChange={(event) => setDraft({ ...draft, model_id: event.target.value || undefined })}
            />
          </Form.Item>
          <Form.Item label="转发结果" htmlFor="call-filter-4">
            <Select
              id="call-filter-4"
              className={styles.select}
              placeholder="转发结果"
              allowClear
              value={draft.outcome}
              onChange={(outcome) => setDraft({ ...draft, outcome })}
              options={Object.entries(outcomeLabels).map(([value, label]) => ({ value, label }))}
            />
          </Form.Item>
          <Form.Item label="调用类型" htmlFor="call-filter-5">
            <Select
              id="call-filter-5"
              className={styles.select}
              placeholder="调用类型"
              allowClear
              value={draft.kind}
              onChange={(kind) => setDraft({ ...draft, kind })}
              options={[
                { value: 'proxy', label: '模型调用' },
                { value: 'admin_test', label: '配置测试' },
              ]}
            />
          </Form.Item>
          <Form.Item label="调用时间" htmlFor="call-filter-6" className={styles.time_filter}>
            <DatePicker.RangePicker
              className={styles.time_range}
              id="call-filter-6"
              showTime
              placeholder={['开始时间（含）', '结束时间（不含）']}
              onChange={(values) =>
                setDraft({ ...draft, from: values?.[0]?.toISOString(), to: values?.[1]?.toISOString() })
              }
            />
          </Form.Item>
          <Form.Item>
            <Space>
              <Button type="primary" htmlType="submit">
                查询
              </Button>
              <Button onClick={records.reload}>刷新</Button>
            </Space>
          </Form.Item>
        </Form>
        {(records.error || settings.error) && <Alert type="error" title={records.error || settings.error} />}
        <div ref={ref} className={styles.list}>
          <Table
            rowKey="id"
            columns={columns}
            dataSource={records.value?.items ?? []}
            loading={records.loading}
            scroll={{ x: 1290, y: Math.max(120, height - 120) }}
            pagination={false}
            footer={() => (
              <div className={styles.pagination}>
                <Pagination
                  current={(query.offset ?? 0) / (query.limit ?? 50) + 1}
                  pageSize={query.limit ?? 50}
                  total={records.value?.total ?? 0}
                  showSizeChanger={false}
                  onChange={(page) => setQuery({ ...query, offset: (page - 1) * (query.limit ?? 50) })}
                />
              </div>
            )}
          />
        </div>
      </Card>
      {settingsOpen && settings.value && (
        <RecordingDialog initial={settings.value} onClose={() => setSettingsOpen(false)} onSaved={settings.reload} />
      )}
    </section>
  );
});
