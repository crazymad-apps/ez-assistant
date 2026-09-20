import { useState } from 'react';
import { Alert, Button, Card, Form, Input } from 'antd';
import { LockOutlined, UserOutlined } from '@ant-design/icons';
import styles from './index.module.scss';

type LoginPageProps = Readonly<{
  notice: string;
  onLogin: (username: string, password: string) => Promise<string | undefined>;
}>;

export function LoginPage(props: LoginPageProps) {
  const [form] = Form.useForm<{ username: string; password: string }>();
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  return (
    <main className={styles.page}>
      <Card variant="borderless" className={styles.card} aria-label="管理员登录">
        <div className={styles.brand}>
          <span className={styles.brand_icon}>EZ</span>
          <h1>ez-assistant 企业中心</h1>
        </div>
        {props.notice && <Alert title={props.notice} type="info" showIcon className={styles.alert} />}
        {error && <Alert title={error} type="error" showIcon className={styles.alert} />}
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
              const result = await props.onLogin(values.username, values.password);
              if (result) setError(result);
            } finally {
              form.resetFields(['password']);
              setPending(false);
            }
          }}
        >
          <Form.Item name="username" label="账号" rules={[{ required: true, message: '请输入账号' }]}>
            <Input size="large" prefix={<UserOutlined />} placeholder="输入登录账号" autoComplete="username" />
          </Form.Item>
          <Form.Item name="password" label="密码" rules={[{ required: true, message: '请输入密码' }]}>
            <Input.Password
              size="large"
              prefix={<LockOutlined />}
              placeholder="输入密码"
              autoComplete="current-password"
            />
          </Form.Item>
          <Button type="primary" size="large" htmlType="submit" loading={pending} block>
            登录
          </Button>
        </Form>
        <div className={styles.help}>仅供企业管理员使用，普通用户请通过 Runtime 登录。</div>
      </Card>
      <p className={styles.footer}>企业管理后台</p>
    </main>
  );
}
