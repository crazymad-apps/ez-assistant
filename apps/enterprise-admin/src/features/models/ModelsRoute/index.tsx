import { useState } from 'react';
import { useNavigate } from 'react-router';
import { observer } from 'mobx-react-lite';
import { Alert, App, Button, Card, Space, Table, Tag, Typography } from 'antd';
import adminStore from '../../../stores/AdminStore';
import { modelApi } from '../../../request/models';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { providerLabels, reasons } from '../model';
import { DefaultModelDialog } from '../DefaultModelDialog';
import styles from './index.module.scss';

const read = async (token: string, signal: AbortSignal) => {
  const [providers, settings, templates] = await Promise.all([
    modelApi.providers(token, signal),
    modelApi.settings(token, signal),
    modelApi.templates(token, signal),
  ]);
  return { providers, settings, templates };
};

export const ModelsRoute = observer(function ModelsRoute() {
  const query = useResourceQuery(read),
    navigate = useNavigate(),
    { modal, message } = App.useApp();
  const [selecting, setSelecting] = useState(false);
  const data = query.value;

  async function reloadTemplates() {
    const error = await adminStore.reloadModelTemplates();
    query.reload();
    if (error) void message.error(error);
    else void message.success('模板已刷新');
  }

  return (
    <section className={styles.page}>
      {query.error && (
        <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重新读取</Button>} />
      )}
      <Card title="企业默认模型" loading={!data && query.loading}>
        <Space wrap>
          <Typography.Text>{data?.settings.default_model?.model_id ?? '尚未选择默认模型'}</Typography.Text>
          <Tag color={data?.settings.state === 'ready' ? 'success' : 'warning'}>
            {data?.settings.state === 'ready' ? '可用' : (reasons[data?.settings.reason ?? 'not_selected'] ?? '不可用')}
          </Tag>
          <Button disabled={adminStore.busy || !data} onClick={() => setSelecting(true)}>
            选择默认模型
          </Button>
          <Button
            disabled={adminStore.busy || !data?.settings.default_model}
            onClick={() =>
              modal.confirm({
                title: '清除默认模型？',
                content: '新的企业模型请求将不可用，已接纳的流不因本次操作主动中断。',

                onOk: async () => {
                  const error = await adminStore.setDefaultModel(null);
                  if (error) {
                    void message.error(error);
                    throw new Error(error);
                  }
                  query.reload();
                },
              })
            }
          >
            清除
          </Button>
        </Space>
      </Card>
      <Card
        title="服务商"
        className={styles.providers}
        extra={
          <Space wrap>
            <Button disabled={adminStore.busy} onClick={reloadTemplates}>
              刷新模板
            </Button>
            <Button type="primary" disabled={adminStore.busy} onClick={() => navigate('/models/providers/new')}>
              添加服务商
            </Button>
          </Space>
        }
      >
        {data && !data.templates.available && <Alert type="warning" title="模板暂不可用，请刷新模板后重试。" />}
        <Table
          rowKey="provider_instance_id"
          size="small"
          loading={query.loading}
          dataSource={data?.providers}
          pagination={false}
          scroll={{ x: 700, y: 320 }}
          columns={[
            { title: '名称', render: (_, row) => row.connection.display_name },
            { title: '类型', render: (_, row) => providerLabels[row.connection.provider_type] },
            { title: '服务地址', ellipsis: true, render: (_, row) => row.connection.endpoint },
            { title: '凭据', render: (_, row) => (row.has_api_key ? '已设置' : '未设置') },
            {
              title: '操作',
              width: 100,
              fixed: 'right',
              align: 'center',

              render: (_, row) => (
                <Button
                  type="link"
                  size="small"
                  onClick={() => navigate(`/models/providers/${row.provider_instance_id}`)}
                >
                  管理
                </Button>
              ),
            },
          ]}
        />
      </Card>
      {selecting && data && (
        <DefaultModelDialog
          providers={data.providers}
          onClose={() => setSelecting(false)}
          onSaved={() => {
            setSelecting(false);
            query.reload();
          }}
        />
      )}
    </section>
  );
});
