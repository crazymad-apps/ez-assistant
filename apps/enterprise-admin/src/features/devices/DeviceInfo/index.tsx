import type { ReactNode } from 'react';
import { Card, Descriptions, Flex, Typography } from 'antd';
import type { HostDeviceDetail } from '../../../request/openapi';
import { statusLabels } from '../model';

export function DeviceInfo(props: { readonly device: HostDeviceDetail; readonly nameEditor?: ReactNode }) {
  return (
    <Card title="设备信息">
      <Flex vertical gap={16}>
        {props.nameEditor}
        <Descriptions
          column={{ xs: 1, sm: 1, lg: 2 }}
          items={[
            ...(!props.nameEditor ? [{ key: 'name', label: '设备名称', children: props.device.name }] : []),
            { key: 'id', label: '设备编号', children: <Typography.Text copyable>{props.device.id}</Typography.Text> },
            { key: 'installation', label: '安装身份', children: props.device.installation_id },
            { key: 'status', label: '登记状态', children: statusLabels[props.device.status] },
            { key: 'enabled', label: '管理状态', children: props.device.enabled ? '启用' : '停用' },
            { key: 'online', label: '连接状态', children: props.device.online ? '在线' : '离线' },
            {
              key: 'platform',
              label: '平台 / Host 版本',
              children: `${props.device.platform} / ${props.device.host_version}`,
            },
            { key: 'applicant', label: '申请人', children: `#${props.device.applicant_user_id}` },
            { key: 'created', label: '申请时间', children: new Date(props.device.created_at).toLocaleString() },
          ]}
        />
      </Flex>
    </Card>
  );
}
