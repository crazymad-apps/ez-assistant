import { Navigate } from 'react-router';
import type { RouteObject } from 'react-router';
import { Button, Result } from 'antd';
import { useNavigate } from 'react-router';
import { AdminApp } from './AdminApp';
import { PageLayout } from './PageLayout';
import { LoginRoute, RequireIdentity } from './IdentityRoutes';
import { ModelsRoute, ProviderRoute, ProviderEditor, ModelEditor } from '../features/models';
import { UsersRoute } from '../features/users/UsersRoute';
import { CallsRoute, CallDetailRoute } from '../features/calls';
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
                ...[
                  { path: '/models', element: <ModelsRoute />, title: '模型管理' },
                  { path: '/models/providers/new', element: <ProviderEditor />, title: '添加服务商' },
                  { path: '/models/providers/:id', element: <ProviderRoute />, title: '服务商详情' },
                  { path: '/models/providers/:id/edit', element: <ProviderEditor />, title: '编辑服务商' },
                  { path: '/models/providers/:id/models/new', element: <ModelEditor />, title: '添加模型' },
                  { path: '/models/providers/:id/models/edit', element: <ModelEditor />, title: '模型参数' },
                ].map(({ title, ...route }) => ({
                  ...route,
                  handle: { title, menu: '/models' } satisfies RouteHeading,
                })),
                {
                  path: '/calls',
                  element: <CallsRoute />,
                  handle: { title: '调用记录', menu: '/calls' } satisfies RouteHeading,
                },
                {
                  path: '/calls/:id',
                  element: <CallDetailRoute />,
                  handle: { title: '调用详情', menu: '/calls' } satisfies RouteHeading,
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
