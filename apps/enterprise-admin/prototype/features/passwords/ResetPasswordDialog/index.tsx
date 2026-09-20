import { Alert, Form, Input, Modal } from 'antd';
import type { DemoUser } from '../../../model';

type PasswordValues = { password: string; confirmPassword: string };
type ResetPasswordDialogProps = Readonly<{ user: DemoUser; currentId: string; onClose: () => void; onSave: (user: DemoUser, password: string) => void }>;

/** 管理重置不收原密码，包括重置当前管理员自己；个人改密使用另一个组件。 */
export function ResetPasswordDialog(props: ResetPasswordDialogProps) {
  const [form] = Form.useForm<PasswordValues>();
  return <Modal open title="重置用户密码" okText="重置密码" cancelText="取消" onCancel={props.onClose}
    onOk={() => form.submit()} destroyOnHidden>
    <Form form={form} layout="vertical" requiredMark={false} onFinish={values => props.onSave(props.user, values.password)}>
      <Form.Item label="目标用户"><strong>{props.user.displayName} · {props.user.username}</strong></Form.Item>
      <Form.Item name="password" label="新密码" rules={[{ required: true, message: '请输入新密码' }, { min: 12, max: 128, message: '请输入 12–128 位演示密码' }]}>
        <Input.Password autoFocus placeholder="仅用于演示，请勿输入真实密码" autoComplete="new-password" />
      </Form.Item>
      <Form.Item name="confirmPassword" label="确认新密码" dependencies={['password']} rules={[{ required: true, message: '请再次输入新密码' }, ({ getFieldValue }) => ({ validator: (_, value: string) => value === getFieldValue('password') ? Promise.resolve() : Promise.reject(new Error('两次密码不一致')) })]}>
        <Input.Password placeholder="再次输入新密码" autoComplete="new-password" />
      </Form.Item>
      <Alert type="warning" showIcon title={props.user.id === props.currentId ? '你正在重置自己的密码，当前登录也将失效。' : '重置后，该用户的全部登录将失效。'} />
    </Form>
  </Modal>;
}
