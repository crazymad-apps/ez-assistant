import { useCallback, useState } from 'react';
import { useNavigate } from 'react-router';
import { Alert, Button, Card, Form, Input, InputNumber, Select, Space, Table, Tag } from 'antd';
import type { TableProps } from 'antd';
import { observer } from 'mobx-react-lite';
import adminStore from '../../../stores/AdminStore';
import type { FeedbackSummary } from '../../../request/openapi';
import { feedbackApi } from '../../../request/feedback';
import type { FeedbackQuery } from '../../../request/feedback';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { kindLabels, kindOptions, sourceLabels, sourceOptions, statusLabels, statusOptions } from '../model';
import styles from './index.module.scss';
import { HandlingDialog } from '../HandlingDialog';

export const FeedbackRoute = observer(function FeedbackRoute() {
  const navigate = useNavigate();
  const [handlingId, setHandlingId] = useState<string | null>(null);
  const [form] = Form.useForm<FeedbackQuery>();
  const kind = Form.useWatch('target_kind', form);
  const unresolved = Form.useWatch('target_unresolved', form);
  const [query, setQuery] = useState<FeedbackQuery>({ limit: 20, offset: 0 });
  const read = useCallback((token: string, signal: AbortSignal) => feedbackApi.list(token, query, signal), [query]);
  const records = useResourceQuery(read);
  const columns: TableProps<FeedbackSummary>['columns'] = [
    { title: '标题', dataIndex: 'title', width: 260, ellipsis: true },
    { title: '反馈目标', width: 220, render: (_, row) => `${kindLabels[row.target_kind]} / ${row.target_name}` },
    { title: '状态', width: 110, render: (_, row) => <Tag>{statusLabels[row.status]}</Tag> },
    { title: '来源', width: 85, render: (_, row) => sourceLabels[row.source] },
    { title: '提交者', width: 160, render: (_, row) => `${row.owner_display_name} (${row.owner_user_id})` },
    { title: '提交时间', width: 190, render: (_, row) => new Date(row.created_at).toLocaleString() },
    {
      title: '操作',
      fixed: 'right',
      align: 'center',
      width: 140,
      render: (_, row) => (
        <Space>
          <Button size="small" disabled={adminStore.busy} onClick={() => void navigate(`/feedback/${row.id}`)}>
            详情
          </Button>
          <Button size="small" disabled={adminStore.busy} onClick={() => setHandlingId(row.id)}>
            处理
          </Button>
        </Space>
      ),
    },
  ];
  return (
    <div className={styles.page}>
      <Card
        title="反馈中心"
        extra={
          <Button onClick={records.reload} disabled={records.loading}>
            刷新
          </Button>
        }
      >
        <Form
          form={form}
          layout="inline"
          className={styles.filters}
          onFinish={(values) => {
            const next = { ...values, limit: 20, offset: 0 };
            if (next.owner_user_id == null) delete next.owner_user_id;
            if (!next.title?.trim()) delete next.title;
            if (!next.target_reference?.trim()) delete next.target_reference;
            setQuery(next);
          }}
        >
          <Form.Item label="状态" name="status">
            <Select allowClear options={statusOptions} className={styles.select} />
          </Form.Item>
          <Form.Item label="来源" name="source">
            <Select allowClear options={sourceOptions} className={styles.select} />
          </Form.Item>
          <Form.Item label="提交者 ID" name="owner_user_id">
            <InputNumber min={1} max={2147483647} precision={0} />
          </Form.Item>
          <Form.Item label="目标类型" name="target_kind">
            <Select
              allowClear
              options={kindOptions}
              className={styles.select}
              onChange={() => form.setFieldsValue({ target_reference: undefined, target_unresolved: undefined })}
            />
          </Form.Item>
          <Form.Item
            label="知识对象"
            name="target_unresolved"
            getValueProps={(value) => ({ value: value === undefined ? undefined : value ? 'manual' : 'referenced' })}
            getValueFromEvent={(value) => (value === undefined ? undefined : value === 'manual')}
          >
            <Select
              allowClear
              onChange={() => form.setFieldsValue({ target_reference: undefined })}
              disabled={kind !== 'knowledge'}
              className={styles.select}
              options={[
                { value: 'manual', label: '手动描述' },
                { value: 'referenced', label: '有稳定引用' },
              ]}
              // Select 使用字符串选项，表单值由 getValueProps/getValueFromEvent 转成契约布尔值。
            />
          </Form.Item>
          <Form.Item label="目标引用" name="target_reference">
            <Input disabled={!kind || unresolved !== undefined} placeholder="精确引用" />
          </Form.Item>
          <Form.Item label="标题" name="title">
            <Input placeholder="包含文字" />
          </Form.Item>
          <Form.Item>
            <Space>
              <Button type="primary" htmlType="submit">
                查询
              </Button>
              <Button
                onClick={() => {
                  form.resetFields();
                  setQuery({ limit: 20, offset: 0 });
                }}
              >
                重置
              </Button>
            </Space>
          </Form.Item>
        </Form>
        {records.error && <Alert type="error" showIcon title={records.error} />}
        <Table
          rowKey="id"
          columns={columns}
          dataSource={records.value?.items ?? []}
          loading={records.loading}
          scroll={{ x: 1100 }}
          locale={{ emptyText: '暂无符合条件的反馈' }}
          pagination={{
            current: Math.floor((query.offset ?? 0) / 20) + 1,
            pageSize: 20,
            total: records.value?.total ?? 0,
            showSizeChanger: false,
            showTotal: (total) => `共 ${total} 条`,
            onChange: (page) => setQuery({ ...query, offset: (page - 1) * 20 }),
          }}
        />
      </Card>
      {handlingId && (
        <HandlingDialog key={handlingId} id={handlingId} onClose={() => setHandlingId(null)} onSaved={records.reload} />
      )}
    </div>
  );
});
