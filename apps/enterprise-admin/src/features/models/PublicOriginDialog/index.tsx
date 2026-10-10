import { useState } from 'react';
import { Alert, App, Form, Input, Modal, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import adminStore from '../../../stores/AdminStore';
import { useLeaveConfirmation } from '../../../app/NavigationGuard/context';

export const PublicOriginDialog = observer(function PublicOriginDialog(props: {
  readonly origin: string | null;
  readonly onClose: () => void;
  readonly onSaved: () => void;
}) {
  const [origin, setOrigin] = useState(props.origin ?? ''),
    [error, setError] = useState('');
  const { modal } = App.useApp();
  const dirty = origin !== (props.origin ?? '');
  useLeaveConfirmation(dirty, props.onClose);
  const close = () => {
    if (adminStore.busy) return;
    if (!dirty) props.onClose();
    else modal.confirm({ title: '放弃未保存的地址？', onOk: props.onClose });
  };
  return (
    <Modal
      open
      title="设置客户端模型访问地址"
      onCancel={close}
      confirmLoading={adminStore.busy}
      cancelButtonProps={{ disabled: adminStore.busy }}
      closable={!adminStore.busy}
      okText="保存"
      okButtonProps={{ htmlType: 'submit', form: 'model-public-origin', disabled: adminStore.busy }}
    >
      {error && <Alert type="error" title={error} />}
      <Form
        id="model-public-origin"
        layout="vertical"
        initialValues={{ origin: props.origin ?? '' }}
        onFinish={async () => {
          const failure = await adminStore.setModelPublicOrigin(origin.trim() || null);
          if (failure) setError(failure);
          else props.onSaved();
        }}
      >
        <Form.Item
          name="origin"
          label="客户端模型访问地址"
          extra="填写客户端可以访问的企业中心地址，例如 http://localhost:7320。留空恢复部署地址。"
          rules={[
            {
              validator: async (_, value: string) => {
                if (!value.trim()) return;
                try {
                  const url = new URL(value.trim());
                  if (
                    ['http:', 'https:'].includes(url.protocol) &&
                    (value.trim() === url.origin || value.trim() === `${url.origin}/`)
                  )
                    return;
                } catch {
                  /* 由下方统一显示输入错误。 */
                }
                throw new Error('请输入完整的 http:// 或 https:// 地址，仅包含主机和端口');
              },
            },
          ]}
        >
          <Input
            aria-label="客户端模型访问地址"
            placeholder="http://localhost:7320"
            disabled={adminStore.busy}
            onChange={(event) => setOrigin(event.target.value)}
          />
        </Form.Item>
      </Form>
      <Typography.Paragraph type="secondary">
        保存后，客户端下次读取模型配置时使用新地址。localhost 仅适用于企业中心与客户端在同一台电脑的情况。
      </Typography.Paragraph>
    </Modal>
  );
});
