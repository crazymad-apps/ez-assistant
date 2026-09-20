import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App as AntApp, ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import { createHashRouter } from 'react-router';
import { RouterProvider } from 'react-router/dom';
import { adminRoutes } from './app/routes';
import { bootstrap } from './bootstrap';
import { adminModalConfig, adminTheme } from './theme';
import 'antd/dist/reset.css';
import './styles.scss';

const { dispose } = bootstrap();
const router = createHashRouter(adminRoutes());
const root = createRoot(document.getElementById('root')!);
root.render(
  <StrictMode>
    <ConfigProvider locale={zhCN} theme={adminTheme} modal={adminModalConfig}>
      <AntApp>
        <RouterProvider router={router} />
      </AntApp>
    </ConfigProvider>
  </StrictMode>,
);

if (import.meta.hot)
  import.meta.hot.dispose(() => {
    router.dispose();
    dispose();
    root.unmount();
  });
