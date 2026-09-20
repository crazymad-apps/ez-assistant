import { observer } from 'mobx-react-lite';
import { Navigate, Outlet, useLocation } from 'react-router';
import adminStore from '../../stores/AdminStore';
import { LoginPage } from '../../features/login/LoginPage';
import { NavigationGuard } from '../NavigationGuard';

export const RequireIdentity = observer(function RequireIdentity() {
  const store = adminStore;
  const location = useLocation();
  if (!store.user) return <Navigate to="/login" replace state={{ returnTo: location.pathname }} />;
  // 不同 Token 必须重新挂载，避免复用前一身份的弹窗和查询结果。
  return (
    <NavigationGuard key={store.token}>
      <Outlet />
    </NavigationGuard>
  );
});

export const LoginRoute = observer(function LoginRoute() {
  const store = adminStore;
  const location = useLocation();
  const state: unknown = location.state;
  const returnTo: unknown = state && typeof state === 'object' ? Reflect.get(state, 'returnTo') : undefined;
  const target =
    typeof returnTo === 'string' && /^\/(?:users|audit(?:\/[1-9][0-9]{0,9})?)$/i.test(returnTo) ? returnTo : '/users';
  if (store.user) return <Navigate to={target} replace />;
  return <LoginPage notice={store.notice} onLogin={store.login} />;
});
