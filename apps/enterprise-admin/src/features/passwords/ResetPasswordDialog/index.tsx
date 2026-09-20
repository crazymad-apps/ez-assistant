import { passwordRule } from '../validation';
import { useState } from 'react';
import { Alert, Form, Input, Modal } from 'antd';
import type { UserView } from '../../../model';

type PasswordValues = { password: string; confirmPassword: string };
type ResetPasswordDialogProps = Readonly<{
  user: UserView;
  currentId: number;
  onClose: () => void;
  onSave: (user: UserView, password: string) => Promise<string | undefined>;
}>;

/** 管理重置不收原密码，包括重置当前管理员自己；个人改密使用另一个组件。 */
export function ResetPasswordDialog(props: ResetPasswordDialogProps) {
  const [form] = Form.useForm<PasswordValues>();
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  return (
    <Modal
      open
      title="重置用户密码"
      okText="重置密码"
      cancelText="取消"
      onCancel={props.onClose}
      onOk={() => form.submit()}
      destroyOnHidden
      confirmLoading={pending}
      closable={!pending}
      keyboard={!pending}
      mask={{ closable: !pending }}
      cancelButtonProps={{ disabled: pending }}
    >
      {error && <Alert type="error" showIcon title={error} />}
      <Form
        form={form}
        layout="vertical"
        requiredMark={false}
        disabled={pending}
        onFinish={async (values) => {
          if (pending) return;
          setPending(true);
          setError(undefined);
          try {
            const result = await props.onSave(props.user, values.password);
            if (result) {
              setError(result);
              form.resetFields();
            }
          } finally {
            setPending(false);
          }
        }}
      >
        <Form.Item label="目标用户">
          <strong>
            {props.user.displayName} · {props.user.username}
          </strong>
        </Form.Item>
        <Form.Item name="password" label="新密码" rules={[{ required: true, message: '请输入新密码' }, passwordRule]}>
          <Input.Password autoFocus placeholder="至少 6 位，包含英文字母和数字" autoComplete="new-password" />
        </Form.Item>
        <Form.Item
          name="confirmPassword"
          label="确认新密码"
          dependencies={['password']}
          rules={[
            { required: true, message: '请再次输入新密码' },
            ({ getFieldValue }) => ({
              validator: (_, value: string) =>
                value === getFieldValue('password') ? Promise.resolve() : Promise.reject(new Error('两次密码不一致')),
            }),
          ]}
        >
          <Input.Password placeholder="再次输入新密码" autoComplete="new-password" />
        </Form.Item>
        <Alert
          type="warning"
          showIcon
          title={
            props.user.id === props.currentId
              ? '你正在重置自己的密码，当前登录也将失效。'
              : '重置后，该用户的全部登录将失效。'
          }
        />
      </Form>
    </Modal>
  );
}
