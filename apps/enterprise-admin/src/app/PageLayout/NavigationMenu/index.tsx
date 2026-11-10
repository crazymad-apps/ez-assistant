import { observer } from 'mobx-react-lite';
import { useMatches, useNavigate } from 'react-router';
import { Menu } from 'antd';
import {
  ApartmentOutlined,
  AuditOutlined,
  BarChartOutlined,
  DesktopOutlined,
  DownloadOutlined,
  FileTextOutlined,
  MessageOutlined,
  RobotOutlined,
  SettingOutlined,
  TeamOutlined,
  ToolOutlined,
} from '@ant-design/icons';
import adminStore from '../../../stores/AdminStore';
import type { RouteHeading } from '../../routes';

const system = [
  { key: '/organizations', icon: <ApartmentOutlined />, label: '组织管理' },
  { key: '/users', icon: <TeamOutlined />, label: '用户管理' },
  { key: '/desktop-releases', icon: <DownloadOutlined />, label: '安装包' },
  { key: '/platform-settings', icon: <SettingOutlined />, label: '平台配置' },
  { key: '/audit', icon: <AuditOutlined />, label: '系统审计' },
];
const organization = [
  { key: '/members', icon: <TeamOutlined />, label: '组织成员' },
  { key: '/plugins', icon: <ToolOutlined />, label: '插件管理' },
  { key: '/agents', icon: <RobotOutlined />, label: '智能体管理' },
  { key: '/models', icon: <SettingOutlined />, label: '模型管理' },
  { key: '/devices', icon: <DesktopOutlined />, label: '设备管理' },
  { key: '/calls', icon: <FileTextOutlined />, label: '调用记录' },
  { key: '/usage', icon: <BarChartOutlined />, label: '用量统计' },
  { key: '/feedback', icon: <MessageOutlined />, label: '反馈中心' },
  { key: '/audit', icon: <AuditOutlined />, label: '组织审计' },
];

/** 主导航从路由元数据取得选中项，不维护另一份页面切换状态。 */
export const NavigationMenu = observer(function NavigationMenu() {
  const navigate = useNavigate();
  const heading = useMatches().at(-1)?.handle as RouteHeading | undefined;
  return (
    <Menu
      aria-label="主导航"
      selectedKeys={[heading?.menu ?? '']}
      disabled={adminStore.busy}
      onClick={({ key }) => void navigate(key)}
      items={adminStore.scope?.kind === 'system' ? system : organization}
    />
  );
});
