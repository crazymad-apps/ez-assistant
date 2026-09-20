import { useState } from 'react';
import { Alert, Button, Card, Form, Input } from 'antd';
import { LockOutlined, UserOutlined } from '@ant-design/icons';
import { DEMO_PASSWORD } from '../../../model';
import styles from './index.module.scss';

type LoginPageProps = Readonly<{ notice: string; onLogin: (username: string, password: string) => string | undefined; onReset: () => void }>;

export function LoginPage(props: LoginPageProps) {
  const [error, setError] = useState<string>();
  return <main className={styles.page}>
    <Card variant="borderless" className={styles.card} aria-label="管理员登录">
      <div className={styles.brand}>
        <span className={styles.brand_icon}>EZ</span>
        <h1>ez-assistant 企业中心</h1>
      </div>
      {props.notice && <Alert title={props.notice} type="info" showIcon className={styles.alert} />}
      {error && <Alert title={error} type="error" showIcon className={styles.alert} />}
      <Form<{ username: string; password: string }> layout="vertical" requiredMark={false} initialValues={{ username: 'admin', password: DEMO_PASSWORD }} onFinish={values => {
        const result = props.onLogin(values.username, values.password); if (result) setError(result);
      }}>
        <Form.Item name="username" label="账号" rules={[{ required: true, message: '请输入账号' }]}><Input size="large" prefix={<UserOutlined />} placeholder="输入演示账号" autoComplete="off" /></Form.Item>
        <Form.Item name="password" label="密码" rules={[{ required: true, message: '请输入密码' }]}><Input.Password size="large" prefix={<LockOutlined />} placeholder="输入演示密码" autoComplete="off" /></Form.Item>
        <Button type="primary" size="large" htmlType="submit" block>登录</Button>
      </Form>
      <div className={styles.help}>演示账号 admin · 密码 {DEMO_PASSWORD}<br />请勿输入真实账号或密码，刷新页面恢复初始数据。</div>
      <Button type="link" size="small" onClick={props.onReset}>重置演示数据</Button>
    </Card>
    <p className={styles.footer}>交互原型 · 仅模拟界面，不连接真实服务</p>
  </main>;
}
