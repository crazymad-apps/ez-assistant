import { useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Alert, Button, Card, Table, Tag, Typography } from 'antd';
import { api } from '../../../request/api';
import { useResourceQuery } from '../../../request/useResourceQuery';
import type { Organization } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { OrganizationEditor } from '../OrganizationEditor';

export const OrganizationsRoute = observer(function OrganizationsRoute() {
  const query = useResourceQuery(api.organizations);
  const [editing, setEditing] = useState<Organization | 'new'>();
  return (
    <section>
      <Card
        title={<Typography.Title level={4}>组织管理</Typography.Title>}
        extra={
          <Button type="primary" disabled={adminStore.busy} onClick={() => setEditing('new')}>
            新建组织
          </Button>
        }
      >
        {query.error && (
          <Alert type="error" message={query.error} action={<Button onClick={query.reload}>重试</Button>} />
        )}
        <Table<Organization>
          rowKey="id"
          loading={query.loading}
          dataSource={query.value}
          columns={[
            { title: '组织名称', dataIndex: 'name' },
            { title: '组织编码', dataIndex: 'code' },
            {
              title: '状态',
              render: (_, org) => (
                <Tag color={org.enabled ? 'success' : undefined}>{org.enabled ? '启用' : '停用'}</Tag>
              ),
            },
            {
              title: '操作',
              render: (_, org) => (
                <Button type="link" onClick={() => setEditing(org)}>
                  编辑
                </Button>
              ),
            },
          ]}
        />
      </Card>
      {editing && (
        <OrganizationEditor
          organization={editing === 'new' ? undefined : editing}
          onClose={() => setEditing(undefined)}
          onSaved={() => {
            setEditing(undefined);
            query.reload();
          }}
        />
      )}
    </section>
  );
});
