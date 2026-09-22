import { passwordRule } from '../../passwords/validation';
import { useState } from 'react';
import { App, Alert, Form, Input, Modal, Typography } from 'antd';
import styles from './index.module.scss';

type PasswordForm = { oldPassword: string; newPassword: string; confirmPassword: string };
type PasswordDialogProps = Readonly<{
  onClose: () => void;
  onSave: (oldPassword: string, newPassword: string) => Promise<string | undefined>;
}>;

/** 修改本人密码弹窗：校验原密码；管理重置他人密码使用另一个组件。 */
export function PasswordDialog(props: PasswordDialogProps) {
  const { message } = App.useApp();
  const [form] = Form.useForm<PasswordForm>();
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  return (
    <Modal
      open
      title="修改密码"
      okText="修改密码"
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
      {error && <Alert showIcon type="error" title={error} className={styles.alert} />}
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
            const result = await props.onSave(values.oldPassword, values.newPassword);
            if (result) {
              setError(result);
              form.resetFields();
            } else {
              form.resetFields();
              void message.success('密码已修改');
              props.onClose();
            }
          } finally {
            setPending(false);
          }
        }}
      >
        <Form.Item name="oldPassword" label="原密码" rules={[{ required: true, message: '请输入原密码' }]}>
          <Input.Password placeholder="输入当前密码" autoComplete="current-password" />
        </Form.Item>
        <Form.Item
          name="newPassword"
          label="新密码"
          rules={[{ required: true, message: '请输入新密码' }, passwordRule]}
        >
          <Input.Password placeholder="至少 6 位，包含英文字母和数字" autoComplete="new-password" />
        </Form.Item>
        <Form.Item
          name="confirmPassword"
          label="确认新密码"
          dependencies={['newPassword']}
          rules={[
            { required: true, message: '请再次输入新密码' },
            ({ getFieldValue }) => ({
              validator: (_, value: string) =>
                value === getFieldValue('newPassword')
                  ? Promise.resolve()
                  : Promise.reject(new Error('两次密码不一致')),
            }),
          ]}
        >
          <Input.Password placeholder="再次输入新密码" autoComplete="new-password" />
        </Form.Item>
        <Typography.Text type="secondary" className={styles.hint}>
          修改成功后保留当前登录，后续登录请使用新密码。
        </Typography.Text>
      </Form>
    </Modal>
  );
}
