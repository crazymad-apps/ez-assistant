import { useCallback, useState } from 'react';
import { Alert, Button, Form, Input, Modal, Select, Space, Switch } from 'antd';
import { observer } from 'mobx-react-lite';
import type { OrganizationMember, SaveMember } from '../../../request/openapi';
import { api } from '../../../request/api';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';

export const MemberEditor = observer(function MemberEditor(props: {
  readonly member?: OrganizationMember;
  readonly onClose: () => void;
  readonly onSaved: () => void;
}) {
  const { member, onClose, onSaved } = props;
  const [form] = Form.useForm<SaveMember>();
  const [username, setUsername] = useState('');
  const [lookup, setLookup] = useState('');
  const [error, setError] = useState('');
  const [dirty, setDirty] = useState(false);
  useLeaveConfirmation(dirty, onClose);

  const read = useCallback(
    (token: string, signal: AbortSignal) =>
      lookup ? api.memberCandidate(token, lookup, signal) : Promise.resolve(undefined),
    [lookup],
  );
  const candidate = useResourceQuery(read);
  const target = member?.user_id ?? candidate.value?.id;

  async function submit(input: SaveMember) {
    if (!target) return;
    setError('');
    const problem = await adminStore.manage((token) => api.saveMember(token, target, input), onSaved);
    if (problem) setError(problem);
  }

  return (
    <Modal
      open
      title={member ? `编辑成员 · ${member.display_name}` : '添加成员'}
      onCancel={onClose}
      onOk={() => form.submit()}
      okText="保存"
      confirmLoading={adminStore.busy}
      okButtonProps={{ disabled: !target }}
      cancelButtonProps={{ disabled: adminStore.busy }}
    >
      {!member && (
        <Space orientation="vertical" size="small">
          <Space.Compact>
            <Input
              aria-label="账号"
              placeholder="输入完整账号"
              value={username}
              onChange={(e) => {
                setUsername(e.target.value);
                setLookup('');
                setDirty(true);
              }}
              onPressEnter={() => setLookup(username.trim())}
            />
            <Button
              disabled={!username.trim()}
              loading={Boolean(lookup) && candidate.loading}
              onClick={() => {
                if (lookup === username.trim()) candidate.reload();
                else setLookup(username.trim());
              }}
            >
              查询
            </Button>
          </Space.Compact>
          {candidate.error && <Alert type="error" message={candidate.error} />}
          {candidate.value && (
            <span>
              {candidate.value.display_name}（{candidate.value.username}）
            </span>
          )}
        </Space>
      )}
      {error && <Alert type="error" message={error} />}
      <Form<SaveMember>
        layout="vertical"
        form={form}
        onFinish={submit}
        onValuesChange={() => setDirty(true)}
        initialValues={member ?? { role: 'member', enabled: true }}
      >
        <Form.Item label="组织角色" name="role" rules={[{ required: true }]}>
          <Select
            options={[
              { value: 'member', label: '普通成员' },
              { value: 'admin', label: '组织管理员' },
            ]}
          />
        </Form.Item>
        <Form.Item label="启用成员" name="enabled" valuePropName="checked">
          <Switch />
        </Form.Item>
      </Form>
    </Modal>
  );
});
