import { DesktopOutlined } from '@ant-design/icons';
import { Badge, Button, Card, Space, Tag, Typography } from 'antd';
import { Link } from 'react-router';
import type { HostDeviceSummary } from '../../../request/openapi';
import { statusLabels } from '../model';
import styles from './index.module.scss';

export function DeviceCard(props: {
  device: HostDeviceSummary;
  disabled: boolean;
  onEdit: (id: string, reject?: boolean) => void;
}) {
  const { device } = props;
  return (
    <Card size="small">
      <div className={styles.content}>
        <div className={styles.heading}>
          <Space>
            <DesktopOutlined />
            <Typography.Text type="secondary">{device.platform}</Typography.Text>
          </Space>
          <Badge status={device.online ? 'success' : 'default'} text={device.online ? '在线' : '离线'} />
        </div>
        <Link className={styles.name} to={`/devices/${device.id}`} title={device.name}>
          {device.name}
        </Link>
        <div className={styles.metadata}>
          <Space wrap>
            {device.status !== 'registered' && (
              <Tag color={device.status === 'pending' ? 'processing' : 'default'}>{statusLabels[device.status]}</Tag>
            )}
            {device.status === 'registered' && (
              <Tag color={device.enabled ? 'success' : 'warning'}>{device.enabled ? '已启用' : '已停用'}</Tag>
            )}
          </Space>
          {device.status === 'pending' ? (
            <Typography.Text type="secondary">申请于 {new Date(device.created_at).toLocaleString()}</Typography.Text>
          ) : (
            <Typography.Text type="secondary">可用用户 {device.user_count ?? '—'} 人</Typography.Text>
          )}
        </div>
        <div className={styles.actions}>
          <Space>
            {device.status === 'pending' && (
              <Button size="small" type="primary" disabled={props.disabled} onClick={() => props.onEdit(device.id)}>
                确认登记
              </Button>
            )}
            {device.status === 'pending' && (
              <Button size="small" danger disabled={props.disabled} onClick={() => props.onEdit(device.id, true)}>
                拒绝
              </Button>
            )}
          </Space>
          <Link to={`/devices/${device.id}`}>详情</Link>
        </div>
      </div>
    </Card>
  );
}
