import { observer } from 'mobx-react-lite';
import { Result } from 'antd';
import type { RouteHeading } from '../routes';
import { Navigate, Outlet, useMatches } from 'react-router';
import adminStore from '../../stores/AdminStore';
import { LoginPage } from '../../features/login/LoginPage';
import { SelectInitialScope } from '../ScopePicker';
import { NavigationGuard } from '../NavigationGuard';

export const RequireIdentity = observer(function RequireIdentity() {
  const store = adminStore;
  if (!store.scope?.can_manage) return <Navigate to="/login" replace />;
  // 不同 Token 必须重新挂载，避免复用前一身份的弹窗和查询结果。
  return (
    <NavigationGuard key={store.token}>
      <Outlet />
    </NavigationGuard>
  );
});

export const LoginRoute = observer(function LoginRoute() {
  const store = adminStore;
  if (store.scope?.can_manage)
    return <Navigate to={store.scope.kind === 'system' ? '/organizations' : '/members'} replace />;
  return (
    <LoginPage notice={store.notice} onLogin={store.login}>
      {store.user && <SelectInitialScope key={store.token} />}
    </LoginPage>
  );
});

const systemMenus = new Set(['/organizations', '/users', '/desktop-releases', '/platform-settings', '/audit']);

const organizationMenus = new Set([
  '/members',
  '/plugins',
  '/agents',
  '/models',
  '/devices',
  '/calls',
  '/usage',
  '/feedback',
  '/audit',
]);

export const ScopedOutlet = observer(function ScopedOutlet() {
  const heading = useMatches().at(-1)?.handle as RouteHeading | undefined;
  const system = adminStore.scope?.kind === 'system';
  if (!heading) return <Navigate to={system ? '/organizations' : '/members'} replace />;
  if (heading.menu === '') return <Result status="404" title="页面不存在" />;
  if (system ? systemMenus.has(heading.menu) : organizationMenus.has(heading.menu)) return <Outlet />;
  return <Result status="403" title="此页面在当前管理范围不可用" subTitle="请通过顶栏的切换组织入口选择管理范围。" />;
});
