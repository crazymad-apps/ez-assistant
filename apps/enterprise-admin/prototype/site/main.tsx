import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import { Home } from './Home';
import { createHashRouter, RouterProvider, Navigate, Link } from 'react-router';
import { SiteShell } from './SiteShell';
import { Changelog } from './Changelog';
import { Manual } from './Manual';
import { siteTheme } from './theme';
import 'antd/dist/reset.css';
import './styles.scss';

const router = createHashRouter([
  {
    element: <SiteShell />,
    children: [
      { path: '/', element: <Home /> },
      { path: '/changelog', element: <Changelog /> },
      { path: '/manual', element: <Navigate to="/manual/getting-started" replace /> },
      { path: '/manual/:articleId', element: <Manual /> },
      {
        path: '*',
        element: (
          <main id="main">
            <h1>页面不存在</h1>
            <Link to="/">返回首页</Link>
          </main>
        ),
      },
    ],
  },
]);

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ConfigProvider locale={zhCN} theme={siteTheme}>
      <RouterProvider router={router} />
    </ConfigProvider>
  </StrictMode>,
);
