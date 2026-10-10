import { observer } from 'mobx-react-lite';
import { useMatches, useNavigate } from 'react-router';
import { Menu } from 'antd';
import {
  AuditOutlined,
  DownloadOutlined,
  CommentOutlined,
  TeamOutlined,
  ApiOutlined,
  RobotOutlined,
  AppstoreOutlined,
  BarChartOutlined,
} from '@ant-design/icons';
import adminStore from '../../../stores/AdminStore';
import type { RouteHeading } from '../../routes';

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
      items={[
        { key: '/users', icon: <TeamOutlined />, label: '用户管理' },
        { key: '/models', icon: <ApiOutlined />, label: '模型管理' },
        { key: '/agents', icon: <RobotOutlined />, label: '智能体管理' },
        { key: '/plugins', icon: <AppstoreOutlined />, label: '插件管理' },
        { key: '/desktop-releases', icon: <DownloadOutlined />, label: '安装包' },
        { key: '/feedback', icon: <CommentOutlined />, label: '反馈中心' },
        { key: '/calls', icon: <ApiOutlined />, label: '调用记录' },
        { key: '/usage', icon: <BarChartOutlined />, label: '用量统计' },
        { key: '/audit', icon: <AuditOutlined />, label: '管理审计' },
      ]}
    />
  );
});
