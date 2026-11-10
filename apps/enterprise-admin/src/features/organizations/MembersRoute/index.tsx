import { useCallback, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Alert, App, Button, Card, Input, Select, Space, Table, Tag, Typography } from 'antd';
import { api } from '../../../request/api';
import { useResourceQuery } from '../../../request/useResourceQuery';
import type { OrganizationMember } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { MemberEditor } from '../MemberEditor';
import styles from './index.module.scss';

export const MembersRoute = observer(function MembersRoute() {
  const store = adminStore;
  const canWrite = store.user?.is_super_admin || store.user?.role === 'system_admin';
  const { modal, message } = App.useApp();
  const [editing, setEditing] = useState<OrganizationMember | 'new'>();
  const [search, setSearch] = useState('');
  const [role, setRole] = useState<OrganizationMember['role']>();
  const [enabled, setEnabled] = useState<boolean>();
  const [page, setPage] = useState(1);
  const read = useCallback(
    (token: string, signal: AbortSignal) =>
      api.members(token, { search, role, enabled, limit: 20, offset: (page - 1) * 20 }, signal),
    [enabled, page, role, search],
  );
  const query = useResourceQuery(read);

  function remove(member: OrganizationMember) {
    modal.confirm({
      title: `移除成员 ${member.display_name}？`,
      content: '该账号将失去本组织的成员权限。',
      okText: '移除',
      okButtonProps: { danger: true },
      onOk: async () => {
        const problem = await store.manage((token) => api.removeMember(token, member.user_id), query.reload);
        if (problem) void message.error(problem);
      },
    });
  }

  return (
    <section>
      <Card
        title={<Typography.Title level={4}>组织成员</Typography.Title>}
        extra={
          canWrite && (
            <Button type="primary" disabled={store.busy} onClick={() => setEditing('new')}>
              添加成员
            </Button>
          )
        }
      >
        <Space className={styles.filters} wrap>
          <Input.Search
            aria-label="搜索成员"
            placeholder="账号或姓名"
            allowClear
            maxLength={64}
            onSearch={(value) => {
              setSearch(value);
              setPage(1);
            }}
          />
          <Select
            aria-label="组织角色筛选"
            placeholder="全部角色"
            allowClear
            style={{ width: 144 }}
            value={role}
            onChange={(value) => {
              setRole(value);
              setPage(1);
            }}
            options={[
              { value: 'member', label: '普通成员' },
              { value: 'admin', label: '组织管理员' },
            ]}
          />
          <Select
            aria-label="成员状态筛选"
            placeholder="全部状态"
            allowClear
            style={{ width: 124 }}
            value={enabled === undefined ? undefined : String(enabled)}
            onChange={(value) => {
              setEnabled(value === undefined ? undefined : value === 'true');
              setPage(1);
            }}
            options={[
              { value: 'true', label: '启用' },
              { value: 'false', label: '停用' },
            ]}
          />
        </Space>
        {query.error && (
          <Alert type="error" message={query.error} action={<Button onClick={query.reload}>重试</Button>} />
        )}
        <Table<OrganizationMember>
          rowKey="user_id"
          loading={query.loading}
          dataSource={query.value?.items}
          pagination={{
            current: page,
            pageSize: 20,
            showSizeChanger: false,
            total: query.value?.total,
            onChange: setPage,
          }}
          columns={[
            { title: '账号', dataIndex: 'username' },
            { title: '显示名称', dataIndex: 'display_name' },
            { title: '组织角色', render: (_, member) => (member.role === 'admin' ? '组织管理员' : '普通成员') },
            { title: '账号状态', render: (_, member) => <Tag>{member.account_enabled ? '启用' : '停用'}</Tag> },
            { title: '成员状态', render: (_, member) => <Tag>{member.enabled ? '启用' : '停用'}</Tag> },
            ...(canWrite
              ? [
                  {
                    title: '操作',
                    render: (_: unknown, member: OrganizationMember) => (
                      <Space>
                        <Button type="link" disabled={store.busy} onClick={() => setEditing(member)}>
                          编辑
                        </Button>
                        <Button type="link" danger disabled={store.busy} onClick={() => remove(member)}>
                          移除
                        </Button>
                      </Space>
                    ),
                  },
                ]
              : []),
          ]}
        />
      </Card>
      {editing && (
        <MemberEditor
          member={editing === 'new' ? undefined : editing}
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
