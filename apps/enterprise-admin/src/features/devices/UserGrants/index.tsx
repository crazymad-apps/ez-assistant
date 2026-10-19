import { useState } from 'react';
import { PlusOutlined } from '@ant-design/icons';
import { Button, Switch, Table, Tag, Typography } from 'antd';
import type { HostDeviceGrant, IdentityUser } from '../../../request/openapi';
import { AddUserDialog } from './AddUserDialog';
import styles from './index.module.scss';

/** 授权表保存完整草稿，候选搜索和翻页不改变已添加用户。 */
export function UserGrants(props: {
  users: HostDeviceGrant[];
  identities: IdentityUser[];
  disabled: boolean;
  onChange: (users: HostDeviceGrant[]) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [addedIdentities, setAddedIdentities] = useState<IdentityUser[]>([]);
  const identity = (id: number) =>
    props.identities.find((user) => user.id === id) ?? addedIdentities.find((user) => user.id === id);

  function add(user: IdentityUser) {
    if (
      props.disabled ||
      !user.enabled ||
      props.users.length >= 100 ||
      props.users.some((row) => row.user_id === user.id)
    )
      return;
    setAddedIdentities((current) => [...current.filter((row) => row.id !== user.id), user]);
    props.onChange([...props.users, { user_id: user.id, desktop_view: false, desktop_control: false }]);
  }

  function permission(id: number, capability: 'desktop_view' | 'desktop_control', checked: boolean) {
    props.onChange(
      props.users.map((row) => {
        if (row.user_id !== id) return row;
        if (capability === 'desktop_view')
          return { ...row, desktop_view: checked, desktop_control: checked && row.desktop_control };
        return { ...row, desktop_control: checked, desktop_view: checked || row.desktop_view };
      }),
    );
  }

  return (
    <div className={styles.section}>
      <div className={styles.toolbar}>
        <Typography.Text type="secondary">已添加 {props.users.length} 人</Typography.Text>
        <Button
          icon={<PlusOutlined />}
          disabled={props.disabled || props.users.length >= 100}
          onClick={() => setAdding(true)}
        >
          添加用户
        </Button>
      </div>
      <Table<HostDeviceGrant>
        size="small"
        rowKey="user_id"
        dataSource={props.users}
        pagination={false}
        scroll={{ x: 560, y: 300 }}
        locale={{ emptyText: '暂无可用用户，请添加用户' }}
        columns={[
          {
            title: '用户',
            width: 220,
            render: (_, row) => {
              const user = identity(row.user_id);
              return (
                <div className={styles.identity}>
                  <Typography.Text>
                    {user?.display_name ?? `用户 #${row.user_id}`}
                    {user && !user.enabled && <Tag>已停用</Tag>}
                  </Typography.Text>
                  <Typography.Text type="secondary">{user?.username}</Typography.Text>
                </div>
              );
            },
          },
          {
            title: '查看桌面',
            width: 110,
            align: 'center',
            render: (_, row) => (
              <Switch
                size="small"
                aria-label={`查看桌面 ${identity(row.user_id)?.display_name ?? row.user_id}`}
                checked={row.desktop_view}
                disabled={props.disabled}
                onChange={(checked) => permission(row.user_id, 'desktop_view', checked)}
              />
            ),
          },
          {
            title: '控制桌面',
            width: 110,
            align: 'center',
            render: (_, row) => (
              <Switch
                size="small"
                aria-label={`控制桌面 ${identity(row.user_id)?.display_name ?? row.user_id}`}
                checked={row.desktop_control}
                disabled={props.disabled}
                onChange={(checked) => permission(row.user_id, 'desktop_control', checked)}
              />
            ),
          },
          {
            title: '操作',
            width: 80,
            fixed: 'right',
            align: 'center',
            render: (_, row) => (
              <Button
                size="small"
                type="text"
                danger
                disabled={props.disabled}
                onClick={() => props.onChange(props.users.filter((user) => user.user_id !== row.user_id))}
              >
                移除
              </Button>
            ),
          },
        ]}
      />
      <Typography.Text type="secondary">
        添加后可使用该设备；控制桌面同时开启查看权限。修改在保存后生效，远程桌面功能尚未开放。
      </Typography.Text>
      {adding && (
        <AddUserDialog
          selected={props.users.map((user) => user.user_id)}
          disabled={props.disabled}
          onAdd={add}
          onClose={() => setAdding(false)}
        />
      )}
    </div>
  );
}
