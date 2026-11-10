import { useState } from 'react';
import { observer } from 'mobx-react-lite';
import { Alert, App, Button, Card, Space, Typography } from 'antd';
import adminStore from '../../../stores/AdminStore';
import { modelApi } from '../../../request/models';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { PublicOriginDialog } from '../PublicOriginDialog';

const read = async (token: string, signal: AbortSignal) => {
  const [settings, templates] = await Promise.all([
    modelApi.settings(token, signal),
    modelApi.templates(token, signal),
  ]);
  return { settings, templates };
};

export const PlatformSettingsRoute = observer(function PlatformSettingsRoute() {
  const query = useResourceQuery(read);
  const { message } = App.useApp();
  const [editingOrigin, setEditingOrigin] = useState(false);
  const data = query.value;

  async function reloadTemplates() {
    const error = await adminStore.reloadModelTemplates();
    query.reload();
    if (error) void message.error(error);
    else void message.success('模板已刷新');
  }

  return (
    <Space orientation="vertical" size="middle" style={{ width: '100%' }}>
      {query.error && (
        <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重新读取</Button>} />
      )}
      <Card title="客户端模型访问地址" loading={!data && query.loading}>
        <Space wrap>
          <Typography.Text>{data?.settings.public_origin ?? '尚未配置'}</Typography.Text>
          <Button disabled={adminStore.busy || !data} onClick={() => setEditingOrigin(true)}>
            设置地址
          </Button>
        </Space>
      </Card>
      <Card title="模型模板" loading={!data && query.loading}>
        <Space wrap>
          <Typography.Text>{data?.templates.available ? '模板可用' : '模板暂不可用'}</Typography.Text>
          <Button disabled={adminStore.busy} onClick={reloadTemplates}>
            刷新模板
          </Button>
        </Space>
      </Card>
      {editingOrigin && data && (
        <PublicOriginDialog
          origin={data.settings.public_origin}
          onClose={() => setEditingOrigin(false)}
          onSaved={() => {
            setEditingOrigin(false);
            query.reload();
            void message.success('客户端模型访问地址已保存');
          }}
        />
      )}
    </Space>
  );
});
