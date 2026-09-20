import { observer } from 'mobx-react-lite';
import { useMatches, useNavigate } from 'react-router';
import { Menu } from 'antd';
import { AuditOutlined, TeamOutlined } from '@ant-design/icons';
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
        { key: '/audit', icon: <AuditOutlined />, label: '管理审计' },
      ]}
    />
  );
});
