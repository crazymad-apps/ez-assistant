import { Navigate } from 'react-router';
import type { RouteObject } from 'react-router';
import { Button, Result } from 'antd';
import { useNavigate } from 'react-router';
import { AdminApp } from './AdminApp';
import { PageLayout } from './PageLayout';
import { LoginRoute, RequireIdentity } from './IdentityRoutes';
import { UsersRoute } from '../features/users/UsersRoute';
import { AuditRoute } from '../features/audit/AuditRoute';
import { AuditDetailRoute } from '../features/audit/AuditDetailRoute';

export type RouteHeading = Readonly<{ title: string; menu: string }>;

function MissingRoute() {
  const navigate = useNavigate();
  return (
    <Result
      status="404"
      title="页面不存在"
      extra={<Button onClick={() => navigate('/users', { replace: true })}>返回用户管理</Button>}
    />
  );
}

/** 页面只有路由这一份导航状态；弹窗草稿不进入 URL/history，身份守卫不替代服务端鉴权。 */
export function adminRoutes(): RouteObject[] {
  return [
    {
      element: <AdminApp />,
      children: [
        { path: '/login', element: <LoginRoute /> },
        {
          element: <RequireIdentity />,
          children: [
            {
              element: <PageLayout />,
              children: [
                { index: true, element: <Navigate to="/users" replace /> },
                {
                  path: '/users',
                  element: <UsersRoute />,
                  handle: { title: '用户管理', menu: '/users' } satisfies RouteHeading,
                },
                {
                  path: '/audit',
                  element: <AuditRoute />,
                  handle: { title: '管理审计', menu: '/audit' } satisfies RouteHeading,
                },
                {
                  path: '/audit/:id',
                  element: <AuditDetailRoute />,
                  handle: { title: '审计详情', menu: '/audit' } satisfies RouteHeading,
                },
                {
                  path: '*',
                  element: <MissingRoute />,
                  handle: { title: '页面不存在', menu: '' } satisfies RouteHeading,
                },
              ],
            },
          ],
        },
      ],
    },
  ];
}
