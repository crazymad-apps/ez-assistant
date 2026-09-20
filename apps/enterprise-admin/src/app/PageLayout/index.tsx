import { Outlet } from 'react-router';
import { Layout } from 'antd';
import { NavigationMenu } from './NavigationMenu';
import { PageBreadcrumb } from './PageBreadcrumb';
import { UserMenu } from './UserMenu';
import styles from './index.module.scss';

/** 页面布局只负责区域排布；导航、路由标题与账号交互由各子组件就近承载。 */
export function PageLayout() {
  return (
    <Layout className={styles.shell}>
      <Layout.Header className={styles.header}>
        <div className={styles.brand}>
          <span className={styles.brand_icon}>EZ</span>
          <strong>ez-assistant 企业中心</strong>
        </div>
        <div className={styles.header_actions}>
          <UserMenu />
        </div>
      </Layout.Header>
      <Layout>
        <Layout.Sider width={200} className={styles.sider}>
          <NavigationMenu />
        </Layout.Sider>
        <Layout.Content className={styles.content}>
          <PageBreadcrumb />
          <Outlet />
        </Layout.Content>
      </Layout>
    </Layout>
  );
}
