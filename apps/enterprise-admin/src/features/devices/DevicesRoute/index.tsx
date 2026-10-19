import { useState } from 'react';
import { App, Button, Card, Input, Space } from 'antd';
import { observer } from 'mobx-react-lite';
import adminStore from '../../../stores/AdminStore';
import { useDeviceRefresh } from '../useDeviceRefresh';
import { DeviceSection } from '../DeviceSection';
import { DeviceEditor } from '../DeviceEditor';
import styles from './index.module.scss';

export const DevicesRoute = observer(function DevicesRoute() {
  const { message } = App.useApp();
  const [search, setSearch] = useState('');
  const [refresh, setRefresh] = useState(0);
  const [editor, setEditor] = useState<{ id: string; reject?: boolean }>();
  useDeviceRefresh(() => setRefresh((n) => n + 1), !!editor || adminStore.busy);
  const edit = (id: string, reject?: boolean) => setEditor({ id, reject });
  return (
    <div className={styles.page}>
      <Card>
        <Space wrap>
          <Input.Search
            aria-label="搜索设备名称或编号"
            placeholder="设备名称或编号"
            maxLength={80}
            allowClear
            onSearch={setSearch}
            disabled={adminStore.busy}
          />
          <Button onClick={() => setRefresh((n) => n + 1)} disabled={adminStore.busy}>
            刷新
          </Button>
        </Space>
      </Card>
      <DeviceSection
        key={`registered:${search}`}
        status="registered"
        title="已登记设备"
        search={search}
        refresh={refresh}
        onEdit={edit}
      />
      <DeviceSection
        key={`unregistered:${search}`}
        status="unregistered"
        title="未登记设备"
        search={search}
        refresh={refresh}
        onEdit={edit}
      />
      {editor && (
        <DeviceEditor
          {...editor}
          onClose={() => setEditor(undefined)}
          onSaved={() => {
            setRefresh((n) => n + 1);
            void message.success(editor.reject ? '申请已拒绝' : '设备已更新，已登记设备可在已登记分栏查看');
          }}
        />
      )}
    </div>
  );
});
