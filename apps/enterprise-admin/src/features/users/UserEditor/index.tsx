import { passwordRule } from '../../passwords/validation';
import { useState } from 'react';
import { Alert, App, Descriptions, Form, Input, Modal, Radio, Tag } from 'antd';
import type { UserView, UserDraft } from '../../../model';
import styles from './index.module.scss';

type UserEditorProps = Readonly<{
  user?: UserView;
  onBack: () => void;
  onSave: (draft: UserDraft, user?: UserView) => Promise<string | undefined>;
}>;

/** 创建/编辑用户弹窗：表单较小，不再占用二级页面；底部按钮使用 Modal 原生 footer。 */
export function UserEditor(props: UserEditorProps) {
  const [form] = Form.useForm<UserDraft & { confirmPassword: string }>();
  const { modal } = App.useApp();
  const [error, setError] = useState<string>();
  const [pending, setPending] = useState(false);
  function close() {
    if (pending) return;
    if (!form.isFieldsTouched()) {
      props.onBack();
      return;
    }
    modal.confirm({ title: '放弃尚未保存的修改？', okText: '放弃修改', cancelText: '继续编辑', onOk: props.onBack });
  }
  function save(values: UserDraft) {
    const commit = async () => {
      if (pending) return;
      setPending(true);
      setError(undefined);
      try {
        const result = await props.onSave(values, props.user);
        if (result) {
          setError(result);
          form.resetFields(['password', 'confirmPassword']);
        }
      } finally {
        setPending(false);
      }
    };
    if (props.user && values.role !== props.user.role) {
      modal.confirm({
        title: `修改 ${props.user.username} 的角色？`,
        content: '该用户的全部登录将失效。',
        okText: '确认修改',
        cancelText: '取消',
        onOk: commit,
      });
    } else commit();
  }
  return (
    <Modal
      open
      width={520}
      mask={{ closable: false }}
      onCancel={close}
      confirmLoading={pending}
      closable={!pending}
      keyboard={!pending}
      cancelButtonProps={{ disabled: pending }}
      okText={props.user ? '保存修改' : '创建用户'}
      cancelText="取消"
      // 通过原生 form 属性让 Modal footer 的确定按钮提交内部表单。
      okButtonProps={{ htmlType: 'submit', form: 'user-editor-form' }}
      title={
        <span className={styles.title}>
          {props.user ? '编辑用户' : '创建用户'}
          {props.user?.superAdmin && <Tag color="gold">超级管理员</Tag>}
        </span>
      }
    >
      {error && <Alert type="warning" showIcon title={error} className={styles.alert} />}
      {props.user && (
        <Descriptions
          className={styles.meta}
          size="small"
          column={1}
          items={[{ key: 'created', label: '创建时间', children: props.user.createdAt }]}
        />
      )}
      <Form
        id="user-editor-form"
        form={form}
        layout="vertical"
        onFinish={save}
        requiredMark={false}
        disabled={pending}
        initialValues={props.user ?? { role: 'user' }}
      >
        <Form.Item
          name="username"
          label="登录账号"
          rules={[
            { required: true, message: '请输入登录账号' },
            { pattern: /^[a-zA-Z0-9._-]{3,64}$/, message: '使用 3–64 位字母、数字或 . _ -' },
          ]}
        >
          <Input placeholder="例如 zhang.san" disabled={Boolean(props.user)} autoComplete="off" />
        </Form.Item>
        <Form.Item
          name="displayName"
          label="显示名称"
          rules={[
            { required: true, whitespace: true, message: '请输入显示名称' },
            { max: 64, message: '名称不超过 64 个字符' },
          ]}
        >
          <Input placeholder="例如 张三" />
        </Form.Item>
        <Form.Item name="role" label="用户角色">
          <Radio.Group
            className={styles.role_options}
            options={[
              { value: 'user', label: '普通用户', disabled: props.user?.superAdmin && props.user.role === 'admin' },
              { value: 'admin', label: '管理员' },
            ]}
          />
        </Form.Item>
        {!props.user && (
          <>
            <Form.Item
              name="password"
              label="初始密码"
              rules={[{ required: true, message: '请输入密码' }, passwordRule]}
            >
              <Input.Password placeholder="至少 6 位，包含英文字母和数字" autoComplete="new-password" />
            </Form.Item>
            <Form.Item
              name="confirmPassword"
              label="确认密码"
              dependencies={['password']}
              rules={[
                { required: true, message: '请再次输入密码' },
                ({ getFieldValue }) => ({
                  validator: (_, value: string) =>
                    value === getFieldValue('password')
                      ? Promise.resolve()
                      : Promise.reject(new Error('两次密码不一致')),
                }),
              ]}
            >
              <Input.Password placeholder="再次输入密码" autoComplete="new-password" />
            </Form.Item>
            <p className={styles.hint}>新建用户默认启用；初始密码由管理员通过企业内部渠道交付。</p>
          </>
        )}
      </Form>
    </Modal>
  );
}
