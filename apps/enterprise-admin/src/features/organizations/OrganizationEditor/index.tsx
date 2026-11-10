import { useCallback, useState } from 'react';
import { Alert, Form, Input, Modal, Select, Switch } from 'antd';
import { observer } from 'mobx-react-lite';
import { api } from '../../../request/api';
import { useResourceQuery } from '../../../request/useResourceQuery';
import type { Organization, UpdateOrganization } from '../../../request/openapi';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';

type Draft = UpdateOrganization & { administrator_user_id?: number };

function AdministratorSelect({
  value,
  onChange,
}: {
  readonly value?: number;
  readonly onChange?: (id: number) => void;
}) {
  const [search, setSearch] = useState('');
  const read = useCallback(
    (token: string, signal: AbortSignal) => api.users(token, { search, enabled: true, limit: 20 }, signal),
    [search],
  );
  const accounts = useResourceQuery(read);
  return (
    <Select
      aria-label="首位组织管理员"
      placeholder="搜索账号或姓名"
      value={value}
      onChange={onChange}
      showSearch={{ filterOption: false, onSearch: setSearch }}
      loading={accounts.loading}
      notFoundContent={accounts.error || '没有匹配的启用账号'}
      options={accounts.value?.items.map((user) => ({
        value: user.id,
        label: `${user.display_name}（${user.username}）`,
      }))}
    />
  );
}

export const OrganizationEditor = observer(function OrganizationEditor(props: {
  readonly organization?: Organization;
  readonly onClose: () => void;
  readonly onSaved: () => void;
}) {
  const { organization, onClose, onSaved } = props;
  const [form] = Form.useForm<Draft>();
  const [error, setError] = useState('');
  const [dirty, setDirty] = useState(false);
  useLeaveConfirmation(dirty, onClose);

  async function submit(input: Draft) {
    setError('');
    const problem = await adminStore.manage(
      (token) =>
        organization
          ? api.updateOrganization(token, organization.id, {
              code: input.code,
              name: input.name,
              enabled: input.enabled,
            })
          : api.createOrganization(token, {
              code: input.code,
              name: input.name,
              administrator_user_id: input.administrator_user_id!,
            }),
      onSaved,
    );
    if (problem) setError(problem);
  }

  return (
    <Modal
      open
      title={organization ? '编辑组织' : '新建组织'}
      onCancel={onClose}
      onOk={() => form.submit()}
      okText="保存"
      confirmLoading={adminStore.busy}
      cancelButtonProps={{ disabled: adminStore.busy }}
    >
      {error && <Alert type="error" message={error} />}
      <Form<Draft>
        layout="vertical"
        form={form}
        initialValues={organization}
        onFinish={submit}
        onValuesChange={() => setDirty(true)}
      >
        <Form.Item label="组织名称" name="name" rules={[{ required: true, whitespace: true }]}>
          <Input maxLength={64} />
        </Form.Item>
        <Form.Item
          label="组织编码"
          name="code"
          rules={[
            {
              required: true,
              pattern: /^[a-z0-9][a-z0-9_-]{0,63}$/,
              message: '请输入 1–64 位小写字母、数字、短横线或下划线，首位为字母或数字。',
            },
          ]}
        >
          <Input maxLength={64} />
        </Form.Item>
        {organization ? (
          <Form.Item label="启用组织" name="enabled" valuePropName="checked">
            <Switch />
          </Form.Item>
        ) : (
          <Form.Item label="首位组织管理员" name="administrator_user_id" rules={[{ required: true }]}>
            <AdministratorSelect />
          </Form.Item>
        )}
      </Form>
    </Modal>
  );
});
