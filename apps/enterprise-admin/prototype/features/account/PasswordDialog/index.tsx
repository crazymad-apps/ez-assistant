import { useState } from 'react';
import { Alert, Form, Input, Modal, Typography } from 'antd';
import styles from './index.module.scss';

type PasswordForm = { oldPassword: string; newPassword: string; confirmPassword: string };
type PasswordDialogProps = Readonly<{ onClose: () => void; onSave: (oldPassword: string, newPassword: string) => string | undefined }>;

/** 修改本人密码弹窗：校验原密码；管理重置他人密码使用另一个组件。 */
export function PasswordDialog(props: PasswordDialogProps) {
  const [form] = Form.useForm<PasswordForm>();
  const [error, setError] = useState<string>();
  return <Modal open title="修改密码" okText="修改密码" cancelText="取消" onCancel={props.onClose}
    onOk={() => form.submit()} destroyOnHidden>
    {error && <Alert showIcon type="error" title={error} className={styles.alert} />}
    <Form form={form} layout="vertical" requiredMark={false} onFinish={values => {
      const result = props.onSave(values.oldPassword, values.newPassword);
      if (result) setError(result);
    }}>
      <Form.Item name="oldPassword" label="原密码" rules={[{ required: true, message: '请输入原密码' }]}>
        <Input.Password placeholder="输入当前演示密码" autoComplete="current-password" />
      </Form.Item>
      <Form.Item name="newPassword" label="新密码" rules={[{ required: true, message: '请输入新密码' }, { min: 12, max: 128, message: '请输入 12–128 位演示密码' }]}>
        <Input.Password placeholder="12–128 位，请勿输入真实密码" autoComplete="new-password" />
      </Form.Item>
      <Form.Item name="confirmPassword" label="确认新密码" dependencies={['newPassword']} rules={[{ required: true, message: '请再次输入新密码' }, ({ getFieldValue }) => ({ validator: (_, value: string) => value === getFieldValue('newPassword') ? Promise.resolve() : Promise.reject(new Error('两次密码不一致')) })]}>
        <Input.Password placeholder="再次输入新密码" autoComplete="new-password" />
      </Form.Item>
      <Typography.Text type="secondary" className={styles.hint}>修改成功后需要重新登录。</Typography.Text>
    </Form>
  </Modal>;
}
