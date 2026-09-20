import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App as AntApp, ConfigProvider } from 'antd';
import zhCN from 'antd/locale/zh_CN';
import { PrototypeApp } from './app/PrototypeApp';
import { adminTheme } from './theme';
import 'antd/dist/reset.css';
import './styles.scss';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ConfigProvider locale={zhCN} theme={adminTheme}>
      <AntApp><PrototypeApp /></AntApp>
    </ConfigProvider>
  </StrictMode>,
);
