import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import { createBrowserRouter, RouterProvider } from 'react-router';
import { Home } from './Home';
import { SiteShell } from './SiteShell';
import { Changelog } from './Changelog';
import { Manual } from './Manual';
import { Download } from './Download';
import { PageState } from './PageState';
import { siteTheme } from './theme';
import 'antd/dist/reset.css';
import './styles.scss';

const router = createBrowserRouter([
  {
    element: <SiteShell />,
    errorElement: <PageState title="页面内容无法显示，请刷新重试" />,
    children: [
      { path: '/', element: <Home /> },
      { path: '/download', element: <Download /> },
      { path: '/changelog', element: <Changelog /> },
      { path: '/manual', element: <Manual /> },
      { path: '/manual/:articleId', element: <Manual /> },
      { path: '*', element: <PageState title="页面不存在" /> },
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
