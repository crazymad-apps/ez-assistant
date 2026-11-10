import { useNavigate } from 'react-router';
import { observer } from 'mobx-react-lite';
import { Alert, Button, Card, Table } from 'antd';
import adminStore from '../../../stores/AdminStore';
import { modelApi } from '../../../request/models';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { providerLabels } from '../model';
import { OrganizationModelSettings } from '../OrganizationModelSettings';
import styles from './index.module.scss';

const read = async (token: string, signal: AbortSignal) => {
  const [providers, settings] = await Promise.all([
    modelApi.providers(token, signal),
    modelApi.settings(token, signal),
  ]);
  return { providers, settings };
};

export const ModelsRoute = observer(function ModelsRoute() {
  const query = useResourceQuery(read),
    navigate = useNavigate();
  const data = query.value;

  return (
    <section className={styles.page}>
      {query.error && (
        <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重新读取</Button>} />
      )}
      {data?.settings.policy ? (
        <OrganizationModelSettings initial={data.settings} providers={data.providers} />
      ) : (
        <Card title="组织配置" loading={query.loading} />
      )}
      <Card
        title="服务商"
        className={styles.providers}
        extra={
          <Button type="primary" disabled={adminStore.busy} onClick={() => navigate('/models/providers/new')}>
            添加服务商
          </Button>
        }
      >
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
    </section>
  );
});
