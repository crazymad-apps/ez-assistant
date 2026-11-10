import { useState } from 'react';
import { Alert, App, Button, Flex, Modal, Select, Table, Tag, Typography } from 'antd';
import { PlusOutlined } from '@ant-design/icons';
import type { TableProps } from 'antd';
import { observer } from 'mobx-react-lite';
import type { UserView } from '../../../model';
import { api } from '../../../request/api';
import type { Organization, SaveMember } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';

const roles = [
  { value: 'member', label: '普通成员' },
  { value: 'admin', label: '组织管理员' },
] satisfies { value: SaveMember['role']; label: string }[];

type MembershipRow = { organization: Organization; member: SaveMember };
type MembershipDraft = { organizationId?: string; role: SaveMember['role'] };

export const OrganizationMembershipDialog = observer(function OrganizationMembershipDialog(
  props: Readonly<{
    user: UserView;
    organizations?: readonly Organization[];
    loading: boolean;
    loadError?: string;
    onReload: () => void;
    onClose: () => void;
    onSaved: () => void;
  }>,
) {
  const { modal } = App.useApp();
  const [memberships, setMemberships] = useState(
    () => new Map<string, SaveMember>(props.user.organizations?.map((item) => [item.id, item])),
  );
  const [savingId, setSavingId] = useState<string>();
  const [error, setError] = useState('');
  const [draft, setDraft] = useState<MembershipDraft>();
  const rows = (props.organizations ?? []).flatMap((organization) => {
    const member = memberships.get(organization.id);
    return member ? [{ organization, member }] : [];
  });
  const available = (props.organizations ?? []).filter(
    (organization) => organization.enabled && !memberships.has(organization.id),
  );
  const selected = available.find((organization) => organization.id === draft?.organizationId);

  async function save(
    organization: Organization,
    work: (token: string) => Promise<SaveMember | void>,
    onSuccess?: () => void,
  ) {
    setError('');
    setSavingId(organization.id);
    const problem = await adminStore.manage(work, (member) => {
      setMemberships((previous) => {
        const next = new Map(previous);
        if (member) next.set(organization.id, member);
        else next.delete(organization.id);
        return next;
      });
      onSuccess?.();
      props.onSaved();
    });
    if (problem) setError(`${organization.name}：${problem}`);
    setSavingId(undefined);
  }

  function remove(organization: Organization) {
    modal.confirm({
      title: `从「${organization.name}」移除 ${props.user.displayName}？`,
      content: '移除后，该用户会失去此组织的成员资格，当前组织会话和设备授权也会撤销。',
      okText: '移除',
      cancelText: '取消',
      okButtonProps: { danger: true },
      onOk: () =>
        save(organization, (token) => api.removeSystemOrganizationMember(token, organization.id, props.user.id)),
    });
  }

  const columns: TableProps<MembershipRow>['columns'] = [
    {
      title: '组织名称',
      key: 'organization',
      render: (_, { organization }) => (
        <Flex align="center" gap={8} wrap>
          <Typography.Text>{organization.name}</Typography.Text>
          {!organization.enabled && <Tag>组织已停用</Tag>}
        </Flex>
      ),
    },
    {
      title: '组织编码',
      key: 'code',
      width: 150,
      ellipsis: true,
      dataIndex: ['organization', 'code'],
    },
    {
      title: '组织角色',
      key: 'role',
      width: 160,
      render: (_, { organization, member }) => (
        <Select<SaveMember['role']>
          aria-label={`${organization.name}的组织角色`}
          disabled={adminStore.busy}
          onChange={(role) =>
            void save(organization, (token) =>
              api.updateSystemOrganizationMember(token, organization.id, props.user.id, { role }),
            )
          }
          options={roles}
          style={{ width: '100%' }}
          value={member.role}
        />
      ),
    },
    {
      title: '操作',
      key: 'actions',
      width: 80,
      align: 'center',
      render: (_, { organization }) => (
        <Button type="link" danger disabled={adminStore.busy} onClick={() => remove(organization)}>
          移除
        </Button>
      ),
    },
  ];

  return (
    <Modal
      open
      width={660}
      title={`${props.user.displayName}的组织归属`}
      onCancel={() => {
        if (!adminStore.busy) props.onClose();
      }}
      footer={
        <Button disabled={adminStore.busy} onClick={props.onClose}>
          关闭
        </Button>
      }
    >
      <Flex vertical gap={12}>
        <Flex justify="end">
          <Button
            type="primary"
            icon={<PlusOutlined />}
            disabled={adminStore.busy || !props.organizations || Boolean(draft)}
            onClick={() => {
              setError('');
              setDraft({ role: 'member' });
            }}
          >
            新增组织
          </Button>
        </Flex>
        {draft && (
          <Flex align="center" gap={8} wrap>
            <Select<string>
              aria-label="要加入的组织"
              placeholder="选择要加入的组织"
              notFoundContent="没有可加入的组织"
              showSearch={{ optionFilterProp: 'label' }}
              disabled={adminStore.busy}
              options={available.map((organization) => ({
                value: organization.id,
                label: `${organization.name}（${organization.code}）`,
              }))}
              style={{ flex: '1 1 220px', minWidth: 0 }}
              value={draft.organizationId}
              onChange={(organizationId) => setDraft({ ...draft, organizationId })}
            />
            <Select<SaveMember['role']>
              aria-label="初始组织角色"
              disabled={adminStore.busy}
              options={roles}
              style={{ width: 144 }}
              value={draft.role}
              onChange={(role) => setDraft({ ...draft, role })}
            />
            <Button
              type="primary"
              disabled={!selected || adminStore.busy}
              loading={Boolean(selected && savingId === selected.id)}
              onClick={() => {
                if (!selected) return;
                void save(
                  selected,
                  (token) =>
                    api.addSystemOrganizationMember(token, selected.id, props.user.id, {
                      role: draft.role,
                      enabled: true,
                    }),
                  () => setDraft(undefined),
                );
              }}
            >
              加入
            </Button>
            <Button disabled={adminStore.busy} onClick={() => setDraft(undefined)}>
              取消
            </Button>
          </Flex>
        )}
        {props.loadError && (
          <Alert
            type="error"
            title={props.loadError}
            action={
              <Button size="small" onClick={props.onReload}>
                重试
              </Button>
            }
          />
        )}
        {error && <Alert type="error" title={error} />}
        <Table<MembershipRow>
          columns={columns}
          dataSource={rows}
          loading={props.loading}
          locale={{ emptyText: '尚未加入组织' }}
          pagination={false}
          rowKey={({ organization }) => organization.id}
          scroll={{ x: 500, y: 360 }}
          size="small"
        />
      </Flex>
    </Modal>
  );
});
